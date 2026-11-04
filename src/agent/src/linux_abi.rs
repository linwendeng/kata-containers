// Copyright (c) 2019 Ant Financial
//
// SPDX-License-Identifier: Apache-2.0
//

use crate::pci;
use anyhow::{Context, Result};
use cfg_if::cfg_if;
#[cfg(any(target_arch = "aarch64", test))]
use std::collections::HashSet;
#[cfg(any(target_arch = "aarch64", test))]
use std::fs;
#[cfg(any(target_arch = "aarch64", test))]
use std::path::{Path, PathBuf};
use std::str::FromStr;
// Linux ABI related constants.

pub const SYSFS_DIR: &str = "/sys";
#[cfg(any(
    all(target_arch = "powerpc64", target_endian = "little"),
    target_arch = "riscv64",
    target_arch = "s390x",
    target_arch = "x86_64",
    target_arch = "x86"
))]
// With NUMA, we need to make sure we use the correct root complex which is
// defined by the pxb-pcie driver.
pub fn create_pci_root_bus_path(root_complex: &str) -> String {
    format!("/devices/pci0000:{root_complex}")
}

const PXB_PCIE_ROOT_BUS_MIN: u8 = 0x20;

pub fn pcipath_from_dev_tree_path(dev_tree_path: &str) -> Result<(&str, pci::Path)> {
    let segments: Vec<&str> = dev_tree_path.split('/').collect();
    // NUMA expander paths include a root bus; ordinary nested slot paths do not.
    let (root_complex, pci_part) = if segments.len() >= 3
        && u8::from_str_radix(segments[0], 16).is_ok_and(|bus| bus >= PXB_PCIE_ROOT_BUS_MIN)
    {
        (segments[0], &dev_tree_path[segments[0].len() + 1..])
    } else {
        ("00", dev_tree_path)
    };
    let pci_path = pci::Path::from_str(pci_part)
        .with_context(|| format!("Failed to parse PCI path from '{}'", dev_tree_path))?;
    Ok((root_complex, pci_path))
}

#[cfg(target_arch = "aarch64")]
pub fn create_pci_root_bus_path(root_complex: &str) -> String {
    let ret = format!("/devices/platform/4010000000.pcie/pci0000:{root_complex}");

    let acpi_root_bus_path = format!("/devices/pci0000:{root_complex}");
    let mut acpi_sysfs_dir = String::from(SYSFS_DIR);
    let mut sysfs_dir = String::from(SYSFS_DIR);
    let mut start_root_bus_path = String::from("/devices/platform/");
    let end_root_bus_path = format!("/pci0000:{root_complex}");

    // check if there is pci bus path for acpi
    acpi_sysfs_dir.push_str(&acpi_root_bus_path);
    if fs::metadata(&acpi_sysfs_dir).is_ok() {
        return acpi_root_bus_path;
    }

    sysfs_dir.push_str(&start_root_bus_path);
    let entries = match fs::read_dir(sysfs_dir) {
        Ok(e) => e,
        Err(_) => return ret,
    };
    for entry in entries {
        let pathname = match entry {
            Ok(p) => p.path(),
            Err(_) => return ret,
        };
        let dir_name = match pathname.file_name() {
            Some(p) => p.to_str(),
            None => return ret,
        };
        let dir_name = match dir_name {
            Some(p) => p,
            None => return ret,
        };
        let dir_name = String::from(dir_name);
        if dir_name.ends_with(".pcie") {
            start_root_bus_path.push_str(&dir_name);
            start_root_bus_path.push_str(&end_root_bus_path);
            return start_root_bus_path;
        }
    }

    ret
}

#[cfg(any(target_arch = "aarch64", test))]
fn matching_pci_root_buses(parent: &Path, root_complex: &str) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", parent.display()))
        }
    };
    let mut buses = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(address) = name.strip_prefix("pci") else {
            continue;
        };
        let Some((segment, bus)) = address.split_once(':') else {
            continue;
        };
        if entry.metadata()?.is_dir()
            && segment.len() == 4
            && segment.chars().all(|c| c.is_ascii_hexdigit())
            && bus.len() == 2
            && bus.eq_ignore_ascii_case(root_complex)
            && bus.chars().all(|c| c.is_ascii_hexdigit())
        {
            buses.push(entry.path());
        }
    }
    Ok(buses)
}

#[cfg(any(target_arch = "aarch64", test))]
fn root_bus_has_downstream_bridge(root_bus: &Path) -> bool {
    let Some(address) = root_bus
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("pci"))
    else {
        return false;
    };
    let Some((segment, bus)) = address.split_once(':') else {
        return false;
    };
    let device_prefix = format!("{segment}:{bus}:");
    let Ok(entries) = fs::read_dir(root_bus) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&device_prefix))
            && entry.path().join("pci_bus").exists()
    })
}

#[cfg(any(target_arch = "aarch64", test))]
fn find_pci_root_bus_path(root_complex: &str, sysfs_devices: &Path) -> Result<PathBuf> {
    let mut candidates = matching_pci_root_buses(sysfs_devices, root_complex)?;
    let platform = sysfs_devices.join("platform");
    let controllers = match fs::read_dir(&platform) {
        Ok(controllers) => Some(controllers),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", platform.display()))
        }
    };
    if let Some(controllers) = controllers {
        for controller in controllers {
            let controller = controller?;
            if controller
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".pcie"))
            {
                candidates.extend(matching_pci_root_buses(&controller.path(), root_complex)?);
            }
        }
    }

    let mut unique_candidates = Vec::new();
    let mut canonical_candidates = HashSet::new();
    for candidate in candidates {
        let canonical = fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
        if canonical_candidates.insert(canonical) {
            unique_candidates.push(candidate);
        }
    }
    let mut candidates = unique_candidates;
    candidates.sort();
    match candidates.len() {
        0 => Err(anyhow::anyhow!(
            "no PCI root bus found for root complex {root_complex}"
        )),
        1 => Ok(candidates.remove(0)),
        _ => {
            let active: Vec<_> = candidates
                .iter()
                .filter(|candidate| root_bus_has_downstream_bridge(candidate))
                .collect();
            if active.len() == 1 {
                Ok(active[0].to_path_buf())
            } else {
                Err(anyhow::anyhow!(
                    "ambiguous PCI root buses for root complex {root_complex}: candidates={candidates:?}, with downstream bridges={active:?}"
                ))
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
pub fn resolve_pci_root_bus_path(root_complex: &str) -> Result<String> {
    let sysfs_root = Path::new(SYSFS_DIR);
    let root_bus = find_pci_root_bus_path(root_complex, &sysfs_root.join("devices"))?;
    let relative = root_bus
        .strip_prefix(sysfs_root)
        .with_context(|| format!("PCI root bus {} is outside sysfs", root_bus.display()))?;
    let relative = relative
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("PCI root bus path is not UTF-8"))?;
    Ok(format!("/{relative}"))
}

#[cfg(not(target_arch = "aarch64"))]
pub fn resolve_pci_root_bus_path(root_complex: &str) -> Result<String> {
    Ok(create_pci_root_bus_path(root_complex))
}

cfg_if! {
    if #[cfg(target_arch = "s390x")] {
        pub const CCW_ROOT_BUS_PATH: &str = "/devices/css0";
        pub const AP_ROOT_BUS_PATH: &str = "/devices/ap";
        pub const AP_SCANS_PATH: &str = "/sys/bus/ap/scans";
        pub const Z9_CRYPT_DEV_PATH: &str = "/dev/z90crypt";
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_numa_root_bus_without_treating_it_as_a_slot() {
        for bus in ["20", "80", "90", "a0", "b0"] {
            let input = format!("{bus}/00/00");
            let (root, path) = pcipath_from_dev_tree_path(&input).unwrap();
            assert_eq!(root, bus);
            assert_eq!(path.to_string(), "00.0/00.0");
        }
    }

    #[test]
    fn preserves_legacy_and_nested_slot_paths() {
        for (input, expected) in [("00/02", "00.0/02.0"), ("02/00/01", "02.0/00.0/01.0")] {
            let (root, path) = pcipath_from_dev_tree_path(input).unwrap();
            assert_eq!(root, "00");
            assert_eq!(path.to_string(), expected);
        }
        assert!(pcipath_from_dev_tree_path("b0/00/20").is_err());
    }

    #[test]
    fn resolves_nonzero_segment_when_duplicate_root_bus_is_empty() {
        let temp = tempdir().unwrap();
        let pcie = temp.path().join("platform/4010000000.pcie");
        let empty = pcie.join("pci0000:80");
        let active = pcie.join("pci0001:80");
        fs::create_dir_all(empty.join("0000:80:00.0")).unwrap();
        fs::create_dir_all(active.join("0001:80:00.0/pci_bus/0001:81")).unwrap();

        assert_eq!(find_pci_root_bus_path("80", temp.path()).unwrap(), active);
    }

    #[test]
    fn rejects_ambiguous_active_root_buses() {
        let temp = tempdir().unwrap();
        let pcie = temp.path().join("platform/4010000000.pcie");
        for segment in ["0000", "0001"] {
            let root = pcie.join(format!("pci{segment}:80"));
            fs::create_dir_all(root.join(format!("{segment}:80:00.0/pci_bus/{segment}:81")))
                .unwrap();
        }

        let error = find_pci_root_bus_path("80", temp.path()).unwrap_err();
        assert!(error.to_string().contains("ambiguous PCI root buses"));
    }
}

// From https://www.kernel.org/doc/Documentation/acpi/namespace.txt
// The Linux kernel's core ACPI subsystem creates struct acpi_device
// objects for ACPI namespace objects representing devices, power resources
// processors, thermal zones. Those objects are exported to user space via
// sysfs as directories in the subtree under /sys/devices/LNXSYSTM:00
pub const ACPI_DEV_PATH: &str = "/devices/LNXSYSTM";

pub const SYSFS_CPU_PATH: &str = "/sys/devices/system/cpu";
pub const SYSFS_CPU_ONLINE_PATH: &str = "/sys/devices/system/cpu/online";

pub const SYSFS_MEMORY_BLOCK_SIZE_PATH: &str = "/sys/devices/system/memory/block_size_bytes";
pub const SYSFS_MEMORY_HOTPLUG_PROBE_PATH: &str = "/sys/devices/system/memory/probe";
pub const SYSFS_MEMORY_ONLINE_PATH: &str = "/sys/devices/system/memory";

pub const SYSFS_SCSI_HOST_PATH: &str = "/sys/class/scsi_host";
pub const SYSFS_NET_PATH: &str = "/sys/class/net";

pub const SYSFS_BUS_PCI_PATH: &str = "/sys/bus/pci";

pub const SYSFS_CGROUPPATH: &str = "/sys/fs/cgroup";
pub const SYSFS_ONLINE_FILE: &str = "online";

pub const PROC_MOUNTSTATS: &str = "/proc/self/mountstats";
pub const PROC_CGROUPS: &str = "/proc/cgroups";

pub const SYSTEM_DEV_PATH: &str = "/dev";

// Linux UEvent related consts.
pub const U_EVENT_ACTION: &str = "ACTION";
pub const U_EVENT_ACTION_ADD: &str = "add";
pub const U_EVENT_ACTION_REMOVE: &str = "remove";
pub const U_EVENT_DEV_PATH: &str = "DEVPATH";
pub const U_EVENT_SUB_SYSTEM: &str = "SUBSYSTEM";
pub const U_EVENT_SEQ_NUM: &str = "SEQNUM";
pub const U_EVENT_DEV_NAME: &str = "DEVNAME";
pub const U_EVENT_INTERFACE: &str = "INTERFACE";
