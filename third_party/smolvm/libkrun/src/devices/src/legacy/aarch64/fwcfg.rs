// Copyright 2026, the libkrun contributors. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use crate::bus::BusDevice;

const FW_CFG_SIGNATURE: u16 = 0x0000;
const FW_CFG_ID: u16 = 0x0001;
const FW_CFG_FILE_DIR: u16 = 0x0019;

const FW_CFG_FILE_FIRST: u16 = 0x0020;

const FW_CFG_FNAME_SIZE: usize = 56;

const DATA_OFFSET: u64 = 0;
const SELECTOR_OFFSET: u64 = 8;

pub struct FwCfg {

    items: BTreeMap<u16, Vec<u8>>,

    files: Vec<(String, u16)>,
    selector: u16,
    offset: usize,
}

impl Default for FwCfg {
    fn default() -> Self {
        Self::new()
    }
}

impl FwCfg {
    pub fn new() -> Self {
        let mut items = BTreeMap::new();
        items.insert(FW_CFG_SIGNATURE, b"QEMU".to_vec());

        items.insert(FW_CFG_ID, 1u32.to_le_bytes().to_vec());
        items.insert(FW_CFG_FILE_DIR, 0u32.to_be_bytes().to_vec());
        Self {
            items,
            files: Vec::new(),
            selector: 0,
            offset: 0,
        }
    }

    pub fn add_file(&mut self, name: &str, content: Vec<u8>) {
        assert!(name.len() < FW_CFG_FNAME_SIZE, "fw_cfg name too long");
        let select = match self.files.iter().find(|(n, _)| n == name) {
            Some((_, s)) => *s,
            None => {
                let s = FW_CFG_FILE_FIRST + self.files.len() as u16;
                self.files.push((name.to_string(), s));
                s
            }
        };
        self.items.insert(select, content);
        self.rebuild_dir();
    }

    fn rebuild_dir(&mut self) {
        let mut dir = (self.files.len() as u32).to_be_bytes().to_vec();
        for (name, select) in &self.files {
            let size = self.items.get(select).map(|c| c.len()).unwrap_or(0) as u32;
            dir.extend_from_slice(&size.to_be_bytes());
            dir.extend_from_slice(&select.to_be_bytes());
            dir.extend_from_slice(&0u16.to_be_bytes());
            let mut fname = [0u8; FW_CFG_FNAME_SIZE];
            fname[..name.len()].copy_from_slice(name.as_bytes());
            dir.extend_from_slice(&fname);
        }
        self.items.insert(FW_CFG_FILE_DIR, dir);
    }
}

impl BusDevice for FwCfg {
    fn read(&mut self, _vcpuid: u64, offset: u64, data: &mut [u8]) {
        log::debug!(
            "fwcfg read: offset={offset} len={} selector={:#06x} item_offset={}",
            data.len(),
            self.selector,
            self.offset
        );
        data.fill(0);
        if offset != DATA_OFFSET {
            return;
        }

        let n = data.len();
        if let Some(item) = self.items.get(&self.selector) {
            for (i, slot) in data.iter_mut().enumerate() {
                *slot = item.get(self.offset + i).copied().unwrap_or(0);
            }
            self.offset += n;
        }
    }

    fn write(&mut self, _vcpuid: u64, offset: u64, data: &[u8]) {
        log::debug!("fwcfg write: offset={offset} data={data:02x?}");

        if offset == SELECTOR_OFFSET && data.len() == 2 {
            self.selector = u16::from_be_bytes([data[0], data[1]]);
            self.offset = 0;
        }

    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn select(dev: &mut FwCfg, key: u16) {
        dev.write(0, SELECTOR_OFFSET, &key.to_be_bytes());
    }

    #[test]
    fn data_reads_preserve_stream_order() {
        let mut dev = FwCfg::new();
        select(&mut dev, FW_CFG_SIGNATURE);
        let mut buf = [0u8; 4];
        dev.read(0, DATA_OFFSET, &mut buf);
        assert_eq!(buf, *b"QEMU");
        let value = u32::from_le_bytes(buf);
        assert_eq!(value, u32::from_le_bytes(*b"QEMU"));
    }

    #[test]
    fn selecting_rewinds_and_reads_advance() {
        let mut dev = FwCfg::new();
        dev.add_file("etc/x", vec![1, 2, 3]);
        let sel = dev.files[0].1;
        select(&mut dev, sel);
        let mut b = [0u8; 1];
        dev.read(0, DATA_OFFSET, &mut b);
        assert_eq!(b[0], 1);
        dev.read(0, DATA_OFFSET, &mut b);
        assert_eq!(b[0], 2);
        select(&mut dev, sel);
        dev.read(0, DATA_OFFSET, &mut b);
        assert_eq!(b[0], 1);

        select(&mut dev, sel);
        let mut b8 = [0xffu8; 8];
        dev.read(0, DATA_OFFSET, &mut b8);
        assert_eq!(&b8[..3], &[1, 2, 3]);
        assert_eq!(&b8[3..], &[0, 0, 0, 0, 0]);
    }

    #[test]
    fn file_dir_lists_files_big_endian() {
        let mut dev = FwCfg::new();
        dev.add_file("etc/table-loader", vec![0; 128]);
        dev.add_file("etc/acpi/tables", vec![0; 512]);
        let dir = dev.items.get(&FW_CFG_FILE_DIR).unwrap();
        assert_eq!(u32::from_be_bytes(dir[..4].try_into().unwrap()), 2);

        assert_eq!(u32::from_be_bytes(dir[4..8].try_into().unwrap()), 128);
        assert_eq!(u16::from_be_bytes(dir[8..10].try_into().unwrap()), 0x20);
        let name = &dir[12..12 + 16];
        assert_eq!(&name[..16], b"etc/table-loader");
    }
}
