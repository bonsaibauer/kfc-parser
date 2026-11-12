//! Reserve every physical source DAT slot before generating new output.
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Error, ErrorKind},
    path::Path,
};

use super::ContainerInfo;

pub(super) struct Reservation {
    pub(super) containers: Vec<ContainerInfo>,
    pub(super) missing_padding: Vec<usize>,
}

pub(super) fn reserve(
    source: &Path,
    stem: &str,
    extension: &str,
    reference: &[ContainerInfo],
) -> io::Result<Reservation> {
    let prefix = format!("{stem}_");
    let suffix = format!(".{extension}");
    let mut occupied = BTreeMap::new();

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(digits) = name
            .strip_prefix(&prefix)
            .and_then(|name| name.strip_suffix(&suffix))
        else {
            continue;
        };
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }

        let index = digits
            .parse::<usize>()
            .map_err(|_| Error::new(ErrorKind::InvalidData, "DAT index overflow"))?;
        if index > u16::MAX as usize || name != format!("{stem}_{index:03}.{extension}") {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "noncanonical or out-of-format source DAT",
            ));
        }

        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "source DAT is not an ordinary file",
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "source DAT is a reparse point",
                ));
            }
        }

        if occupied.insert(index, metadata.len()).is_some() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "duplicate source DAT index",
            ));
        }
    }

    for (index, info) in reference.iter().enumerate() {
        let size = occupied
            .get(&index)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "referenced source DAT is missing"))?;
        if *size < info.size {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "referenced source DAT is shorter than its index entry",
            ));
        }
    }

    let count = occupied
        .last_key_value()
        .map(|(&index, _)| index + 1)
        .unwrap_or(0)
        .max(reference.len());
    let mut containers = reference.to_vec();
    let mut missing_padding = Vec::new();
    for index in containers.len()..count {
        match occupied.get(&index) {
            Some(size) => containers.push(ContainerInfo {
                size: *size,
                count: 0,
            }),
            None => {
                containers.push(ContainerInfo { size: 0, count: 0 });
                missing_padding.push(index);
            }
        }
    }

    Ok(Reservation {
        containers,
        missing_padding,
    })
}
