// SPDX-License-Identifier: MIT
use std::{borrow::Cow, fs::OpenOptions, io::Read, process::ExitCode};

use apple_nvram::{nvram_parse, window::WriteWindow, VarType};

#[derive(Debug)]
#[allow(dead_code)]
enum Error {
    Open(String, std::io::Error),
    Parse,
    SectionTooBig,
    ApplyError(std::io::Error),
    MissingPartitionName,
    MissingValue,
    VariableNotFound,
    UnknownPartition,
    InvalidHex,
}

impl From<apple_nvram::Error> for Error {
    fn from(e: apple_nvram::Error) -> Self {
        match e {
            apple_nvram::Error::ParseError => Error::Parse,
            apple_nvram::Error::SectionTooBig => Error::SectionTooBig,
            apple_nvram::Error::ApplyError(e) => Error::ApplyError(e),
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

fn main() -> ExitCode {
    match real_main() {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{:?}", e);
            ExitCode::FAILURE
        }
    }
}

fn real_main() -> Result<()> {
    let matches = clap::command!()
        .arg(clap::arg!(-d --device [DEVICE] "Path to the nvram device."))
        .subcommand(
            clap::Command::new("read")
                .about("Read nvram variables")
                .arg(clap::Arg::new("variable").num_args(1..)),
        )
        .subcommand(
            clap::Command::new("delete")
                .about("Delete nvram variables")
                .arg(clap::Arg::new("variable").num_args(1..)),
        )
        .subcommand(
            clap::Command::new("write")
                .about("Write nvram variables")
                .arg(clap::Arg::new("variable=value").num_args(1..)),
        )
        .get_matches();
    let default_name = "/dev/mtd/by-name/nvram".to_owned();
    let device = matches.get_one::<String>("device").unwrap_or(&default_name);
    let writing = matches!(matches.subcommand_name(), Some("write") | Some("delete"));
    // Reads open the device read-only, so they work while the boot NOR is
    // read-only and cannot change it.
    let mut file = OpenOptions::new()
        .read(true)
        .write(writing)
        .open(device)
        .map_err(|e| Error::Open(device.clone(), e))?;
    // Writers hold the NOR open for writing from before the read until the
    // change is applied; readers never open it.
    let _window = if writing {
        Some(WriteWindow::open(&file).map_err(Error::ApplyError)?)
    } else {
        None
    };
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|e| Error::Open(device.clone(), e))?;
    let mut nv = nvram_parse(&data)?;
    match matches.subcommand() {
        Some(("read", args)) => {
            let active = nv.active_part_mut();

            let vars = args.get_many::<String>("variable");
            if let Some(vars) = vars {
                for var in vars {
                    let (part, name) = var.split_once(':').ok_or(Error::MissingPartitionName)?;
                    let typ = part_by_name(part)?;
                    let v = active
                        .get_variable(name.as_bytes(), typ)
                        .ok_or(Error::VariableNotFound)?;
                    println!("{}", v);
                }
            } else {
                for var in active.variables() {
                    println!("{}", var);
                }
            }
        }
        Some(("write", args)) => {
            let vars = args.get_many::<String>("variable=value");
            nv.prepare_for_write();
            let active = nv.active_part_mut();
            for var in vars.unwrap_or_default() {
                let (key, value) = var.split_once('=').ok_or(Error::MissingValue)?;
                let (part, name) = key.split_once(':').ok_or(Error::MissingPartitionName)?;
                let typ = part_by_name(part)?;
                active.insert_variable(name.as_bytes(), Cow::Owned(read_var(value)?), typ);
            }
            nv.apply(&mut file)?;
        }
        Some(("delete", args)) => {
            let vars = args.get_many::<String>("variable");
            nv.prepare_for_write();
            let active = nv.active_part_mut();
            for var in vars.unwrap_or_default() {
                let (part, name) = var.split_once(':').ok_or(Error::MissingPartitionName)?;
                let typ = part_by_name(part)?;
                active.remove_variable(name.as_bytes(), typ);
            }
            nv.apply(&mut file)?;
        }
        _ => {}
    }
    Ok(())
}

fn part_by_name(name: &str) -> Result<VarType> {
    match name {
        "common" => Ok(VarType::Common),
        "system" => Ok(VarType::System),
        _ => Err(Error::UnknownPartition),
    }
}

fn read_var(val: &str) -> Result<Vec<u8>> {
    let val = val.as_bytes();
    let mut ret = Vec::new();
    let mut i = 0;
    while i < val.len() {
        if val[i] == b'%' {
            let hex = val.get(i + 1..i + 3).ok_or(Error::InvalidHex)?;
            let hex = std::str::from_utf8(hex).map_err(|_| Error::InvalidHex)?;
            ret.push(u8::from_str_radix(hex, 16).map_err(|_| Error::InvalidHex)?);
            i += 3;
        } else {
            ret.push(val[i]);
            i += 1;
        }
    }
    Ok(ret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_var_decodes_percent_escapes() {
        assert_eq!(read_var("%01").unwrap(), vec![1]);
        assert_eq!(read_var("ab%2fc").unwrap(), b"ab/c".to_vec());
        assert_eq!(read_var("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn read_var_rejects_bad_escapes() {
        for bad in ["%", "%0", "%zz", "a%"] {
            assert!(matches!(read_var(bad), Err(Error::InvalidHex)), "{bad}");
        }
    }

    #[test]
    fn partitions_by_name() {
        assert!(matches!(part_by_name("system"), Ok(VarType::System)));
        assert!(matches!(part_by_name("common"), Ok(VarType::Common)));
        assert!(matches!(part_by_name("other"), Err(Error::UnknownPartition)));
    }
}
