// Copyright 2026, the libkrun contributors. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

pub struct AcpiInfo {

    pub mpidrs: Vec<u64>,

    pub gicd_base: u64,

    pub gicr_base: u64,
    pub gicr_size: u64,

    pub uart: Option<(u64, u32)>,

    pub virtio: Vec<(u64, u64, u32)>,
}

pub struct AcpiBlobs {
    pub rsdp: Vec<u8>,
    pub tables: Vec<u8>,
    pub loader: Vec<u8>,
}

pub const FILE_RSDP: &str = "etc/acpi/rsdp";
pub const FILE_TABLES: &str = "etc/acpi/tables";
pub const FILE_LOADER: &str = "etc/table-loader";

const TIMER_IRQ_SECURE: u32 = 16 + 13;
const TIMER_IRQ_NONSECURE: u32 = 16 + 12;
const TIMER_IRQ_VIRT: u32 = 16 + 11;
const TIMER_IRQ_HYP: u32 = 16 + 14;

const OEM_ID: &[u8; 6] = b"KRUNVM";
const OEM_TABLE_ID: &[u8; 8] = b"KRUNVM  ";
const CREATOR_ID: &[u8; 4] = b"KRUN";

fn put_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put_u64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn header(sig: &[u8; 4], revision: u8) -> Vec<u8> {
    let mut h = Vec::with_capacity(36);
    h.extend_from_slice(sig);
    put_u32(&mut h, 0);
    h.push(revision);
    h.push(0);
    h.extend_from_slice(OEM_ID);
    h.extend_from_slice(OEM_TABLE_ID);
    put_u32(&mut h, 1);
    h.extend_from_slice(CREATOR_ID);
    put_u32(&mut h, 1);
    debug_assert_eq!(h.len(), 36);
    h
}

fn finish_table(t: &mut [u8]) {
    let len = t.len() as u32;
    t[4..8].copy_from_slice(&len.to_le_bytes());
}

fn gas(space: u8, bit_width: u8, access_size: u8, addr: u64) -> Vec<u8> {
    let mut g = vec![space, bit_width, 0, access_size];
    put_u64(&mut g, addr);
    g
}

fn aml_pkg_length(content_len: usize) -> Vec<u8> {

    if content_len + 1 <= 0x3f {
        return vec![(content_len + 1) as u8];
    }
    for extra in 1..=3usize {
        let total = content_len + 1 + extra;
        if total < 1 << (4 + 8 * extra) {
            let mut out = vec![((extra as u8) << 6) | (total & 0xf) as u8];
            let mut rest = total >> 4;
            for _ in 0..extra {
                out.push((rest & 0xff) as u8);
                rest >>= 8;
            }
            return out;
        }
    }
    unreachable!("AML package too large");
}

fn aml_name(seg: &[u8; 4], value: &[u8]) -> Vec<u8> {
    let mut out = vec![0x08];
    out.extend_from_slice(seg);
    out.extend_from_slice(value);
    out
}

fn aml_string(s: &str) -> Vec<u8> {
    let mut out = vec![0x0d];
    out.extend_from_slice(s.as_bytes());
    out.push(0);
    out
}

fn aml_int(n: u64) -> Vec<u8> {
    match n {
        0 => vec![0x00],
        1 => vec![0x01],
        2..=0xff => vec![0x0a, n as u8],
        0x100..=0xffff => {
            let mut v = vec![0x0b];
            v.extend_from_slice(&(n as u16).to_le_bytes());
            v
        }
        _ => {
            let mut v = vec![0x0c];
            v.extend_from_slice(&(n as u32).to_le_bytes());
            v
        }
    }
}

fn aml_virtio_device(index: usize, base: u64, size: u64, gsiv: u32) -> Vec<u8> {

    let mut res: Vec<u8> = Vec::new();
    res.extend_from_slice(&[0x86, 0x09, 0x00, 0x01]);
    put_u32(&mut res, u32::try_from(base).expect("virtio MMIO address exceeds ACPI Memory32Fixed"));
    put_u32(&mut res, u32::try_from(size).expect("virtio MMIO size exceeds ACPI Memory32Fixed"));
    res.extend_from_slice(&[0x89, 0x06, 0x00, 0x01, 0x01]);
    put_u32(&mut res, gsiv);
    res.extend_from_slice(&[0x79, 0x00]);

    let mut buf_content = aml_int(res.len() as u64);
    buf_content.extend_from_slice(&res);
    let mut crs_value = vec![0x11];
    crs_value.extend_from_slice(&aml_pkg_length(buf_content.len()));
    crs_value.extend_from_slice(&buf_content);

    let name = format!("VR{index:02X}");
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(&aml_name(b"_HID", &aml_string("LNRO0005")));
    body.extend_from_slice(&aml_name(b"_UID", &aml_int(index as u64)));
    body.extend_from_slice(&aml_name(b"_CCA", &aml_int(1)));
    body.extend_from_slice(&aml_name(b"_CRS", &crs_value));

    let mut dev = vec![0x5b, 0x82];
    dev.extend_from_slice(&aml_pkg_length(body.len()));
    dev.extend_from_slice(&body);
    dev
}

fn build_dsdt(info: &AcpiInfo) -> Vec<u8> {
    let mut devices: Vec<u8> = Vec::new();
    for (i, (base, size, gsiv)) in info.virtio.iter().enumerate() {
        devices.extend_from_slice(&aml_virtio_device(i, *base, *size, *gsiv));
    }

    let mut scope_body: Vec<u8> = vec![0x5c];
    scope_body.extend_from_slice(b"_SB_");
    scope_body.extend_from_slice(&devices);
    let mut aml = vec![0x10];
    aml.extend_from_slice(&aml_pkg_length(scope_body.len()));
    aml.extend_from_slice(&scope_body);

    let mut t = header(b"DSDT", 2);
    t.extend_from_slice(&aml);
    finish_table(&mut t);
    t
}

fn build_fadt(dsdt_offset: u32) -> Vec<u8> {
    let mut t = header(b"FACP", 6);
    t.resize(276, 0);
    finish_table(&mut t);
    t[112..116].copy_from_slice(&(1u32 << 20).to_le_bytes());
    t[129..131].copy_from_slice(&1u16.to_le_bytes());
    t[131] = 1;
    t[140..148].copy_from_slice(&(dsdt_offset as u64).to_le_bytes());
    t[268..276].copy_from_slice(b"KRUNKRUN");
    t
}

fn build_madt(info: &AcpiInfo) -> Vec<u8> {
    let mut t = header(b"APIC", 5);
    put_u32(&mut t, 0);
    put_u32(&mut t, 0);

    for (i, mpidr) in info.mpidrs.iter().enumerate() {
        let start = t.len();
        t.push(0x0b);
        t.push(80);
        put_u16(&mut t, 0);
        put_u32(&mut t, i as u32);
        put_u32(&mut t, i as u32);
        put_u32(&mut t, 1);
        put_u32(&mut t, 0);
        put_u32(&mut t, 0);
        put_u64(&mut t, 0);
        put_u64(&mut t, 0);
        put_u64(&mut t, 0);
        put_u64(&mut t, 0);
        put_u32(&mut t, 0);
        put_u64(&mut t, 0);
        put_u64(&mut t, *mpidr);
        t.push(0);
        t.push(0);
        put_u16(&mut t, 0);
        debug_assert_eq!(t.len() - start, 80);
    }

    let start = t.len();
    t.push(0x0c);
    t.push(24);
    put_u16(&mut t, 0);
    put_u32(&mut t, 0);
    put_u64(&mut t, info.gicd_base);
    put_u32(&mut t, 0);
    t.push(3);
    t.extend_from_slice(&[0, 0, 0]);
    debug_assert_eq!(t.len() - start, 24);

    let start = t.len();
    t.push(0x0e);
    t.push(16);
    put_u16(&mut t, 0);
    put_u64(&mut t, info.gicr_base);
    put_u32(&mut t, info.gicr_size as u32);
    debug_assert_eq!(t.len() - start, 16);

    finish_table(&mut t);
    t
}

fn build_gtdt() -> Vec<u8> {
    let mut t = header(b"GTDT", 2);
    put_u64(&mut t, !0u64);
    put_u32(&mut t, 0);
    for irq in [
        TIMER_IRQ_SECURE,
        TIMER_IRQ_NONSECURE,
        TIMER_IRQ_VIRT,
        TIMER_IRQ_HYP,
    ] {
        put_u32(&mut t, irq);
        put_u32(&mut t, 0);
    }
    put_u64(&mut t, !0u64);
    put_u32(&mut t, 0);
    put_u32(&mut t, 96);
    finish_table(&mut t);
    debug_assert_eq!(t.len(), 96);
    t
}

fn build_spcr((uart_base, uart_irq): (u64, u32)) -> Vec<u8> {
    let mut t = header(b"SPCR", 2);
    t.push(3);
    t.extend_from_slice(&[0, 0, 0]);
    t.extend_from_slice(&gas(0, 32, 3, uart_base));
    t.push(1 << 3);
    t.push(0);
    put_u32(&mut t, uart_irq);
    t.push(3);
    t.push(0);
    t.push(1);
    t.push(0);
    t.push(0);
    t.push(0);
    put_u16(&mut t, 0xffff);
    put_u16(&mut t, 0xffff);
    t.extend_from_slice(&[0, 0, 0]);
    put_u32(&mut t, 0);
    t.push(0);
    put_u32(&mut t, 0);
    finish_table(&mut t);
    debug_assert_eq!(t.len(), 80);
    t
}

const LOADER_ENTRY_SIZE: usize = 128;
const CMD_ALLOCATE: u32 = 1;
const CMD_ADD_POINTER: u32 = 2;
const CMD_ADD_CHECKSUM: u32 = 3;
const ZONE_HIGH: u8 = 1;
const ZONE_FSEG: u8 = 2;

fn fname(name: &str) -> [u8; 56] {
    let mut f = [0u8; 56];
    f[..name.len()].copy_from_slice(name.as_bytes());
    f
}

fn loader_allocate(out: &mut Vec<u8>, file: &str, align: u32, zone: u8) {
    let start = out.len();
    let mut e = Vec::with_capacity(LOADER_ENTRY_SIZE);
    put_u32(&mut e, CMD_ALLOCATE);
    e.extend_from_slice(&fname(file));
    put_u32(&mut e, align);
    e.push(zone);
    e.resize(LOADER_ENTRY_SIZE, 0);
    out.extend_from_slice(&e);
    debug_assert_eq!(out.len() - start, LOADER_ENTRY_SIZE);
}

fn loader_add_pointer(out: &mut Vec<u8>, pointer_file: &str, pointee_file: &str, offset: u32) {
    let mut e = Vec::with_capacity(LOADER_ENTRY_SIZE);
    put_u32(&mut e, CMD_ADD_POINTER);
    e.extend_from_slice(&fname(pointer_file));
    e.extend_from_slice(&fname(pointee_file));
    put_u32(&mut e, offset);
    e.push(8);
    e.resize(LOADER_ENTRY_SIZE, 0);
    out.extend_from_slice(&e);
}

fn loader_add_checksum(out: &mut Vec<u8>, file: &str, result_offset: u32, start: u32, len: u32) {
    let mut e = Vec::with_capacity(LOADER_ENTRY_SIZE);
    put_u32(&mut e, CMD_ADD_CHECKSUM);
    e.extend_from_slice(&fname(file));
    put_u32(&mut e, result_offset);
    put_u32(&mut e, start);
    put_u32(&mut e, len);
    e.resize(LOADER_ENTRY_SIZE, 0);
    out.extend_from_slice(&e);
}

pub fn build_acpi(info: &AcpiInfo) -> AcpiBlobs {

    let mut tables: Vec<u8> = Vec::new();
    let mut offsets: Vec<(u32, u32)> = Vec::new();
    let mut push = |tables: &mut Vec<u8>, t: Vec<u8>| -> (u32, u32) {
        let off = tables.len() as u32;
        let len = t.len() as u32;
        tables.extend_from_slice(&t);
        offsets.push((off, len));
        (off, len)
    };

    let (dsdt_off, dsdt_len) = push(&mut tables, build_dsdt(info));
    let (fadt_off, fadt_len) = push(&mut tables, build_fadt(dsdt_off));
    let (madt_off, madt_len) = push(&mut tables, build_madt(info));
    let (gtdt_off, gtdt_len) = push(&mut tables, build_gtdt());
    let spcr = info.uart.map(|uart| push(&mut tables, build_spcr(uart)));

    let mut xsdt_entries = vec![fadt_off, madt_off, gtdt_off];
    if let Some((offset, _)) = spcr {
        xsdt_entries.push(offset);
    }
    let mut xsdt = header(b"XSDT", 1);
    for &e in &xsdt_entries {
        put_u64(&mut xsdt, e as u64);
    }
    finish_table(&mut xsdt);
    let xsdt_len = xsdt.len() as u32;
    let xsdt_off = tables.len() as u32;
    tables.extend_from_slice(&xsdt);

    let mut rsdp: Vec<u8> = Vec::new();
    rsdp.extend_from_slice(b"RSD PTR ");
    rsdp.push(0);
    rsdp.extend_from_slice(OEM_ID);
    rsdp.push(2);
    put_u32(&mut rsdp, 0);
    put_u32(&mut rsdp, 36);
    put_u64(&mut rsdp, xsdt_off as u64);
    rsdp.push(0);
    rsdp.extend_from_slice(&[0, 0, 0]);
    debug_assert_eq!(rsdp.len(), 36);
    let sum: u8 = rsdp[..20].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    rsdp[8] = (!sum).wrapping_add(1);

    let mut loader: Vec<u8> = Vec::new();
    loader_allocate(&mut loader, FILE_TABLES, 0x40, ZONE_HIGH);
    loader_add_checksum(&mut loader, FILE_TABLES, dsdt_off + 9, dsdt_off, dsdt_len);
    loader_add_pointer(&mut loader, FILE_TABLES, FILE_TABLES, fadt_off + 140);
    loader_add_checksum(&mut loader, FILE_TABLES, fadt_off + 9, fadt_off, fadt_len);
    loader_add_checksum(&mut loader, FILE_TABLES, madt_off + 9, madt_off, madt_len);
    loader_add_checksum(&mut loader, FILE_TABLES, gtdt_off + 9, gtdt_off, gtdt_len);
    if let Some((offset, len)) = spcr {
        loader_add_checksum(&mut loader, FILE_TABLES, offset + 9, offset, len);
    }
    for i in 0..xsdt_entries.len() as u32 {
        loader_add_pointer(&mut loader, FILE_TABLES, FILE_TABLES, xsdt_off + 36 + i * 8);
    }
    loader_add_checksum(&mut loader, FILE_TABLES, xsdt_off + 9, xsdt_off, xsdt_len);
    loader_allocate(&mut loader, FILE_RSDP, 0x10, ZONE_FSEG);
    loader_add_pointer(&mut loader, FILE_RSDP, FILE_TABLES, 24);
    loader_add_checksum(&mut loader, FILE_RSDP, 32, 0, 36);

    AcpiBlobs {
        rsdp,
        tables,
        loader,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> AcpiInfo {
        AcpiInfo {
            mpidrs: vec![0, 1],
            gicd_base: 0x9fd_0000,
            gicr_base: 0x9fe_0000,
            gicr_size: 0x2_0000,
            uart: Some((0xa00_1000, 33)),
            virtio: vec![(0xa00_2000, 0x1000, 34), (0xa00_3000, 0x1000, 35)],
        }
    }

    #[test]
    fn tables_tile_the_blob() {
        let b = build_acpi(&info());
        let mut off = 0usize;
        let mut sigs = Vec::new();
        while off < b.tables.len() {
            let sig = &b.tables[off..off + 4];
            let len = u32::from_le_bytes(b.tables[off + 4..off + 8].try_into().unwrap()) as usize;
            assert!(len >= 36, "table too short");
            sigs.push(String::from_utf8_lossy(sig).to_string());
            off += len;
        }
        assert_eq!(off, b.tables.len(), "tables must tile the blob exactly");
        assert_eq!(sigs, ["DSDT", "FACP", "APIC", "GTDT", "SPCR", "XSDT"]);
    }

    #[test]
    fn no_serial_omits_spcr_and_its_xsdt_entry() {
        let mut machine = info();
        machine.uart = None;
        let blobs = build_acpi(&machine);
        let mut offset = 0;
        let mut signatures = Vec::new();
        while offset < blobs.tables.len() {
            let length = u32::from_le_bytes(blobs.tables[offset + 4..offset + 8].try_into().unwrap()) as usize;
            signatures.push(&blobs.tables[offset..offset + 4]);
            if &blobs.tables[offset..offset + 4] == b"XSDT" {
                assert_eq!(length, 36 + 3 * 8);
            }
            offset += length;
        }
        assert_eq!(signatures, [b"DSDT", b"FACP", b"APIC", b"GTDT", b"XSDT"]);
    }

    #[test]
    fn loader_is_whole_entries_and_names_match() {
        let b = build_acpi(&info());
        assert_eq!(b.loader.len() % 128, 0);

        assert_eq!(u32::from_le_bytes(b.loader[..4].try_into().unwrap()), 1);
        assert_eq!(&b.loader[4..4 + FILE_TABLES.len()], FILE_TABLES.as_bytes());
    }

    #[test]
    fn rsdp_static_checksum_holds() {
        let b = build_acpi(&info());
        assert_eq!(b.rsdp.len(), 36);
        let sum: u8 = b.rsdp[..20].iter().fold(0u8, |a, x| a.wrapping_add(*x));
        assert_eq!(sum, 0, "first-20-byte checksum must be valid pre-patch");
        assert_eq!(&b.rsdp[..8], b"RSD PTR ");
    }

    #[test]
    fn dsdt_advertises_every_virtio_device() {
        let b = build_acpi(&info());
        let hay = &b.tables;
        let needle = b"LNRO0005";
        let count = hay.windows(needle.len()).filter(|w| w == needle).count();
        assert_eq!(count, 2);
    }

    #[test]
    fn madt_has_gicc_per_cpu_plus_gicd_and_gicr() {
        let b = build_acpi(&info());

        let mut off = 0usize;
        loop {
            let sig = &b.tables[off..off + 4];
            let len = u32::from_le_bytes(b.tables[off + 4..off + 8].try_into().unwrap()) as usize;
            if sig == b"APIC" {
                let mut sub = off + 44;
                let mut kinds = Vec::new();
                while sub < off + len {
                    kinds.push(b.tables[sub]);
                    sub += b.tables[sub + 1] as usize;
                }
                assert_eq!(kinds, [0x0b, 0x0b, 0x0c, 0x0e]);
                return;
            }
            off += len;
        }
    }
}
