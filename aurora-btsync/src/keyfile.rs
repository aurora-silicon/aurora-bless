// SPDX-License-Identifier: MIT
//! A small editor for BlueZ's GLib key files that keeps every line it does
//! not change, so groups and keys BlueZ wrote (services, name, appearance,
//! comments) survive a key refresh byte for byte.

#[derive(Debug, Clone, PartialEq, Eq)]
struct Group {
    /// `None` for lines before the first group header.
    name: Option<String>,
    /// Lines after the header, without line endings.
    lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyFile {
    groups: Vec<Group>,
}

fn header(line: &str) -> Option<&str> {
    line.trim_end().strip_prefix('[')?.strip_suffix(']')
}

fn entry(line: &str) -> Option<(&str, &str)> {
    let t = line.trim_start();
    if t.starts_with('#') {
        return None;
    }
    let (k, v) = t.split_once('=')?;
    Some((k.trim_end(), v.trim_start()))
}

/// Escapes a string value the way `g_key_file_set_string` does.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for (i, c) in value.chars().enumerate() {
        match c {
            ' ' if i == 0 => out.push_str("\\s"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

impl KeyFile {
    pub fn parse(text: &str) -> KeyFile {
        let mut groups = vec![Group {
            name: None,
            lines: Vec::new(),
        }];
        for line in text.lines() {
            if let Some(name) = header(line) {
                groups.push(Group {
                    name: Some(name.to_owned()),
                    lines: Vec::new(),
                });
            } else {
                groups.last_mut().unwrap().lines.push(line.to_owned());
            }
        }
        if groups[0].lines.is_empty() {
            groups.remove(0);
        }
        KeyFile { groups }
    }

    #[cfg(test)]
    pub fn get(&self, group: &str, key: &str) -> Option<&str> {
        self.groups
            .iter()
            .filter(|g| g.name.as_deref() == Some(group))
            .flat_map(|g| g.lines.iter())
            .filter_map(|l| entry(l))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    /// Sets `key` in `group`, keeping the line's place if it exists and
    /// adding the group at the end if it does not.
    pub fn set(&mut self, group: &str, key: &str, value: &str) {
        let line = format!("{key}={value}");
        let pos = self
            .groups
            .iter()
            .position(|g| g.name.as_deref() == Some(group));
        let g = match pos {
            Some(i) => &mut self.groups[i],
            None => {
                self.groups.push(Group {
                    name: Some(group.to_owned()),
                    lines: Vec::new(),
                });
                self.groups.last_mut().unwrap()
            }
        };
        if let Some(l) = g
            .lines
            .iter_mut()
            .find(|l| entry(l).is_some_and(|(k, _)| k == key))
        {
            *l = line;
            return;
        }
        // New keys go after the last non-blank line of the group.
        let at = g
            .lines
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .map_or(0, |i| i + 1);
        g.lines.insert(at, line);
    }

    pub fn remove_group(&mut self, name: &str) {
        self.groups.retain(|g| g.name.as_deref() != Some(name));
    }

    /// Serialises the file; groups are separated by one blank line, as
    /// `g_key_file_to_data` writes them.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for g in &self.groups {
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            if let Some(name) = &g.name {
                out.push('[');
                out.push_str(name);
                out.push_str("]\n");
            }
            let end = g
                .lines
                .iter()
                .rposition(|l| !l.trim().is_empty())
                .map_or(0, |i| i + 1);
            for l in &g.lines[..end] {
                out.push_str(l);
                out.push('\n');
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLUEZ: &str = "[General]\nName=Mouse\nAppearance=0x03c2\nAddressType=static\n\
Services=00001800-0000-1000-8000-00805f9b34fb;00001812-0000-1000-8000-00805f9b34fb;\n\n\
[IdentityResolvingKey]\nKey=00112233445566778899AABBCCDDEEFF\n\n\
[DeviceID]\nSource=2\nVendor=1133\nProduct=45091\nVersion=17\n";

    #[test]
    fn round_trips_unchanged() {
        assert_eq!(KeyFile::parse(BLUEZ).to_text(), BLUEZ);
    }

    #[test]
    fn get_and_set() {
        let mut kf = KeyFile::parse(BLUEZ);
        assert_eq!(kf.get("General", "Name"), Some("Mouse"));
        kf.set("General", "Name", "Other");
        kf.set("LongTermKey", "Key", "AB");
        kf.set("LongTermKey", "EDiv", "0");
        let text = kf.to_text();
        assert!(text.contains("[General]\nName=Other\nAppearance=0x03c2\n"));
        assert!(text.ends_with("Version=17\n\n[LongTermKey]\nKey=AB\nEDiv=0\n"));
    }

    #[test]
    fn remove_group_keeps_the_rest() {
        let mut kf = KeyFile::parse(BLUEZ);
        kf.remove_group("IdentityResolvingKey");
        assert_eq!(
            kf.to_text(),
            BLUEZ.replace("[IdentityResolvingKey]\nKey=00112233445566778899AABBCCDDEEFF\n\n", "")
        );
    }

    #[test]
    fn escapes_like_glib() {
        assert_eq!(escape(" a\\b\nc"), "\\sa\\\\b\\nc");
        assert_eq!(escape("Magic Mouse"), "Magic Mouse");
    }
}
