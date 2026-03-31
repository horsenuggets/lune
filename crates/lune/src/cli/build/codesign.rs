/*!
    Cross-platform ad-hoc code signing for macOS Mach-O binaries.

    Ported from Go's `cmd/internal/codesign` package. This allows signing
    macOS binaries on any platform (Linux, Windows, macOS) without requiring
    Apple's `codesign` tool.

    When `lune build` creates a standalone binary by appending metadata to a
    base Mach-O executable, the original code signature is invalidated. This
    module re-signs the binary with an ad-hoc signature so it won't be killed
    by macOS on Apple Silicon (which requires all ARM64 code to have a valid
    code signature).
*/

use sha2::{Digest, Sha256};

const PAGE_SIZE_BITS: u8 = 12;
const PAGE_SIZE: usize = 1 << PAGE_SIZE_BITS;

// Mach-O constants
const MH_MAGIC_64: u32 = 0xfeedfacf;
const LC_SEGMENT_64: u32 = 0x19;
const LC_CODE_SIGNATURE: u32 = 0x1d;

// Code signature constants
const CSMAGIC_CODEDIRECTORY: u32 = 0xfade0c02;
const CSMAGIC_EMBEDDED_SIGNATURE: u32 = 0xfade0cc0;
const CSSLOT_CODEDIRECTORY: u32 = 0;
const CS_HASHTYPE_SHA256: u8 = 2;
const CS_ADHOC: u32 = 0x2;
const CS_LINKER_SIGNED: u32 = 0x20000;
const CS_EXECSEG_MAIN_BINARY: u64 = 0x1;

const SUPER_BLOB_SIZE: usize = 12;
const BLOB_INDEX_SIZE: usize = 8;
const CODE_DIRECTORY_SIZE: usize = 88;

struct MachOInfo {
    text_offset: u64,
    text_size: u64,
    codesig_cmd_offset: usize,
    codesig_data_offset: u32,
    codesig_data_size: u32,
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

fn write_u32_be(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
}

fn write_u64_be(buf: &mut [u8], offset: usize, val: u64) {
    buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
}

fn write_u32_le(buf: &mut [u8], offset: usize, val: u32) {
    buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

/// Parse a 64-bit Mach-O binary to find the code signature and text segment.
fn parse_macho(data: &[u8]) -> Option<MachOInfo> {
    if data.len() < 32 {
        return None;
    }

    let magic = read_u32_le(data, 0);
    if magic != MH_MAGIC_64 {
        return None;
    }

    let ncmds = read_u32_le(data, 16) as usize;
    // Mach-O 64-bit header is 32 bytes
    let mut offset = 32;

    let mut text_offset = 0u64;
    let mut text_size = 0u64;
    let mut codesig_cmd_offset = 0usize;
    let mut codesig_data_offset = 0u32;
    let mut codesig_data_size = 0u32;

    for _ in 0..ncmds {
        if offset + 8 > data.len() {
            return None;
        }

        let cmd = read_u32_le(data, offset);
        let cmdsize = read_u32_le(data, offset + 4) as usize;

        if cmd == LC_SEGMENT_64 && offset + 72 <= data.len() {
            // segment_command_64: segname at +8, fileoff at +40, filesize at +48
            let segname = &data[offset + 8..offset + 24];
            if segname.starts_with(b"__TEXT\0") {
                text_offset = read_u64_le(data, offset + 40);
                text_size = read_u64_le(data, offset + 48);
            }
        } else if cmd == LC_CODE_SIGNATURE && offset + 16 <= data.len() {
            // linkedit_data_command: dataoff at +8, datasize at +12
            codesig_cmd_offset = offset;
            codesig_data_offset = read_u32_le(data, offset + 8);
            codesig_data_size = read_u32_le(data, offset + 12);
        }

        offset += cmdsize;
    }

    if codesig_data_offset == 0 {
        return None;
    }

    Some(MachOInfo {
        text_offset,
        text_size,
        codesig_cmd_offset,
        codesig_data_offset,
        codesig_data_size,
    })
}

/// Calculate the size of an ad-hoc code signature.
fn signature_size(code_size: u32, id: &[u8]) -> usize {
    let nhashes = (code_size as usize + PAGE_SIZE - 1) / PAGE_SIZE;
    let id_off = CODE_DIRECTORY_SIZE;
    let hash_off = id_off + id.len() + 1; // +1 for null terminator
    let cdir_size = hash_off + nhashes * 32;
    SUPER_BLOB_SIZE + BLOB_INDEX_SIZE + cdir_size
}

/// Build an ad-hoc code signature for the given code content.
fn build_signature(
    code: &[u8],
    id: &[u8],
    text_offset: u64,
    text_size: u64,
    is_main_binary: bool,
) -> Vec<u8> {
    let code_size = code.len() as u32;
    let nhashes = (code.len() + PAGE_SIZE - 1) / PAGE_SIZE;
    let id_off = CODE_DIRECTORY_SIZE as u32;
    let hash_off = id_off + id.len() as u32 + 1;
    let cdir_size = hash_off + nhashes as u32 * 32;
    let total_size = SUPER_BLOB_SIZE + BLOB_INDEX_SIZE + cdir_size as usize;

    let mut buf = vec![0u8; total_size];

    // SuperBlob header
    write_u32_be(&mut buf, 0, CSMAGIC_EMBEDDED_SIGNATURE);
    write_u32_be(&mut buf, 4, total_size as u32);
    write_u32_be(&mut buf, 8, 1); // count = 1 blob

    // BlobIndex entry
    write_u32_be(&mut buf, 12, CSSLOT_CODEDIRECTORY);
    write_u32_be(
        &mut buf,
        16,
        (SUPER_BLOB_SIZE + BLOB_INDEX_SIZE) as u32,
    );

    // CodeDirectory
    let cd_offset = SUPER_BLOB_SIZE + BLOB_INDEX_SIZE;
    write_u32_be(&mut buf, cd_offset, CSMAGIC_CODEDIRECTORY);
    write_u32_be(&mut buf, cd_offset + 4, cdir_size);
    write_u32_be(&mut buf, cd_offset + 8, 0x20400); // version
    write_u32_be(&mut buf, cd_offset + 12, CS_ADHOC | CS_LINKER_SIGNED); // flags
    write_u32_be(&mut buf, cd_offset + 16, hash_off); // hashOffset
    write_u32_be(&mut buf, cd_offset + 20, id_off); // identOffset
    write_u32_be(&mut buf, cd_offset + 24, 0); // nSpecialSlots
    write_u32_be(&mut buf, cd_offset + 28, nhashes as u32); // nCodeSlots
    write_u32_be(&mut buf, cd_offset + 32, code_size); // codeLimit
    buf[cd_offset + 36] = 32; // hashSize (SHA-256)
    buf[cd_offset + 37] = CS_HASHTYPE_SHA256; // hashType
    buf[cd_offset + 39] = PAGE_SIZE_BITS; // pageSize

    // execSeg fields
    write_u64_be(&mut buf, cd_offset + 64, text_offset); // execSegBase
    write_u64_be(&mut buf, cd_offset + 72, text_size); // execSegLimit
    if is_main_binary {
        write_u64_be(&mut buf, cd_offset + 80, CS_EXECSEG_MAIN_BINARY);
    }

    // Identifier (null-terminated)
    let id_start = cd_offset + id_off as usize;
    buf[id_start..id_start + id.len()].copy_from_slice(id);
    buf[id_start + id.len()] = 0;

    // Page hashes
    let hash_start = cd_offset + hash_off as usize;
    for i in 0..nhashes {
        let page_start = i * PAGE_SIZE;
        let page_end = std::cmp::min(page_start + PAGE_SIZE, code.len());
        let hash = Sha256::digest(&code[page_start..page_end]);
        let slot_offset = hash_start + i * 32;
        buf[slot_offset..slot_offset + 32].copy_from_slice(&hash);
    }

    buf
}

/// Ad-hoc sign a Mach-O binary in place. Returns `true` if signing succeeded.
///
/// The binary must already have an `LC_CODE_SIGNATURE` load command (which
/// Rust-compiled macOS binaries always do). This function:
/// 1. Parses the Mach-O headers to find the code signature location
/// 2. Builds a new ad-hoc CodeDirectory with SHA-256 page hashes
/// 3. Writes the signature at the existing code signature offset
/// 4. Updates the load command's datasize if needed
pub fn sign_macho(data: &mut Vec<u8>, id: &str) -> bool {
    let info = match parse_macho(data) {
        Some(info) => info,
        None => return false,
    };

    let code_size = info.codesig_data_offset;
    let sig_offset = code_size as usize;
    let new_sig_size = signature_size(code_size, id.as_bytes());

    // Update LC_CODE_SIGNATURE datasize BEFORE hashing, since the load
    // command lives in the code region (page 0). Updating it after would
    // invalidate the page hash and break idempotency.
    write_u32_le(data, info.codesig_cmd_offset + 12, new_sig_size as u32);

    // Build the signature (hashes the code region which now has the
    // correct datasize value)
    let new_sig = build_signature(
        &data[..code_size as usize],
        id.as_bytes(),
        info.text_offset,
        info.text_size,
        true,
    );

    // Ensure we have enough space
    let new_end = sig_offset + new_sig.len();
    if new_end > data.len() {
        data.resize(new_end, 0);
    }

    // Write the new signature
    data[sig_offset..sig_offset + new_sig.len()].copy_from_slice(&new_sig);

    // Zero any remaining space from the old signature
    if new_sig.len() < info.codesig_data_size as usize {
        let pad_start = sig_offset + new_sig.len();
        let pad_end = sig_offset + info.codesig_data_size as usize;
        if pad_end <= data.len() {
            data[pad_start..pad_end].fill(0);
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid Mach-O 64-bit binary with LC_CODE_SIGNATURE
    /// for testing purposes.
    fn build_test_macho(code_pages: usize) -> Vec<u8> {
        let code_content_size = code_pages * PAGE_SIZE;

        // We need space for: header (32) + LC_SEGMENT_64 for __TEXT (72)
        // + LC_CODE_SIGNATURE (16)
        let header_and_cmds_size = 32 + 72 + 16;

        // Text segment covers from 0 to end of code content
        let text_segment_size = code_content_size;

        // Code signature goes right after the code content
        let codesig_offset = code_content_size;

        // Pre-allocate enough space for the signature (generous estimate)
        let sig_space = 4096;
        let total_size = code_content_size + sig_space;

        let mut data = vec![0u8; total_size];

        // Mach-O 64-bit header
        data[0..4].copy_from_slice(&MH_MAGIC_64.to_le_bytes()); // magic
        data[4..8].copy_from_slice(&0u32.to_le_bytes()); // cputype (don't care)
        data[8..12].copy_from_slice(&0u32.to_le_bytes()); // cpusubtype
        data[12..16].copy_from_slice(&2u32.to_le_bytes()); // filetype MH_EXECUTE
        data[16..20].copy_from_slice(&2u32.to_le_bytes()); // ncmds = 2
        let sizeofcmds: u32 = 72 + 16;
        data[20..24].copy_from_slice(&sizeofcmds.to_le_bytes()); // sizeofcmds
        data[24..28].copy_from_slice(&0u32.to_le_bytes()); // flags
        data[28..32].copy_from_slice(&0u32.to_le_bytes()); // reserved

        // LC_SEGMENT_64 for __TEXT at offset 32
        let cmd_offset = 32;
        data[cmd_offset..cmd_offset + 4]
            .copy_from_slice(&LC_SEGMENT_64.to_le_bytes());
        data[cmd_offset + 4..cmd_offset + 8]
            .copy_from_slice(&72u32.to_le_bytes()); // cmdsize
        data[cmd_offset + 8..cmd_offset + 14].copy_from_slice(b"__TEXT"); // segname
        // vmaddr at +24 = 0
        // vmsize at +32 = text_segment_size
        data[cmd_offset + 32..cmd_offset + 40]
            .copy_from_slice(&(text_segment_size as u64).to_le_bytes());
        // fileoff at +40 = 0
        // filesize at +48 = text_segment_size
        data[cmd_offset + 48..cmd_offset + 56]
            .copy_from_slice(&(text_segment_size as u64).to_le_bytes());

        // LC_CODE_SIGNATURE at offset 32 + 72 = 104
        let cs_cmd_offset = 32 + 72;
        data[cs_cmd_offset..cs_cmd_offset + 4]
            .copy_from_slice(&LC_CODE_SIGNATURE.to_le_bytes());
        data[cs_cmd_offset + 4..cs_cmd_offset + 8]
            .copy_from_slice(&16u32.to_le_bytes()); // cmdsize
        data[cs_cmd_offset + 8..cs_cmd_offset + 12]
            .copy_from_slice(&(codesig_offset as u32).to_le_bytes()); // dataoff
        data[cs_cmd_offset + 12..cs_cmd_offset + 16]
            .copy_from_slice(&(sig_space as u32).to_le_bytes()); // datasize

        // Fill code area with recognizable pattern
        for i in header_and_cmds_size..code_content_size {
            data[i] = (i & 0xff) as u8;
        }

        data
    }

    #[test]
    fn test_parse_macho_valid() {
        let data = build_test_macho(4);
        let info = parse_macho(&data).expect("should parse valid Mach-O");
        assert_eq!(info.text_offset, 0);
        assert_eq!(info.text_size, 4 * PAGE_SIZE as u64);
        assert_eq!(info.codesig_data_offset, 4 * PAGE_SIZE as u32);
    }

    #[test]
    fn test_parse_macho_invalid_magic() {
        let mut data = build_test_macho(1);
        data[0] = 0; // corrupt magic
        assert!(parse_macho(&data).is_none());
    }

    #[test]
    fn test_parse_macho_too_small() {
        let data = vec![0u8; 16];
        assert!(parse_macho(&data).is_none());
    }

    #[test]
    fn test_signature_size_deterministic() {
        let size1 = signature_size(4096, b"test");
        let size2 = signature_size(4096, b"test");
        assert_eq!(size1, size2);

        // Larger code = more page hashes
        let size3 = signature_size(8192, b"test");
        assert!(size3 > size1);

        // Longer identifier = larger signature
        let size4 = signature_size(4096, b"longer-identifier");
        assert!(size4 > size1);
    }

    #[test]
    fn test_signature_structure() {
        let code = vec![0xABu8; PAGE_SIZE * 2];
        let sig = build_signature(&code, b"test", 0, code.len() as u64, true);

        // Verify SuperBlob header
        assert_eq!(
            u32::from_be_bytes(sig[0..4].try_into().unwrap()),
            CSMAGIC_EMBEDDED_SIGNATURE
        );
        assert_eq!(
            u32::from_be_bytes(sig[4..8].try_into().unwrap()),
            sig.len() as u32
        );
        assert_eq!(u32::from_be_bytes(sig[8..12].try_into().unwrap()), 1);

        // Verify BlobIndex
        assert_eq!(
            u32::from_be_bytes(sig[12..16].try_into().unwrap()),
            CSSLOT_CODEDIRECTORY
        );

        // Verify CodeDirectory magic
        let cd_offset = SUPER_BLOB_SIZE + BLOB_INDEX_SIZE;
        assert_eq!(
            u32::from_be_bytes(sig[cd_offset..cd_offset + 4].try_into().unwrap()),
            CSMAGIC_CODEDIRECTORY
        );

        // Verify nCodeSlots = 2 (two pages)
        assert_eq!(
            u32::from_be_bytes(
                sig[cd_offset + 28..cd_offset + 32].try_into().unwrap()
            ),
            2
        );

        // Verify codeLimit
        assert_eq!(
            u32::from_be_bytes(
                sig[cd_offset + 32..cd_offset + 36].try_into().unwrap()
            ),
            (PAGE_SIZE * 2) as u32
        );

        // Verify execSegFlags has MAIN_BINARY set
        assert_eq!(
            u64::from_be_bytes(
                sig[cd_offset + 80..cd_offset + 88].try_into().unwrap()
            ),
            CS_EXECSEG_MAIN_BINARY
        );
    }

    #[test]
    fn test_signature_page_hashes_correct() {
        // Create code with known content
        let mut code = vec![0u8; PAGE_SIZE + 100]; // 1 full page + partial
        for (i, byte) in code.iter_mut().enumerate() {
            *byte = (i % 256) as u8;
        }

        let sig = build_signature(&code, b"x", 0, code.len() as u64, false);

        // Should have 2 hash slots (1 full page + 1 partial)
        let cd_offset = SUPER_BLOB_SIZE + BLOB_INDEX_SIZE;
        let nhashes = u32::from_be_bytes(
            sig[cd_offset + 28..cd_offset + 32].try_into().unwrap(),
        );
        assert_eq!(nhashes, 2);

        // Verify first page hash
        let id_len = 2; // "x" + null
        let hash_start = cd_offset + CODE_DIRECTORY_SIZE + id_len;
        let expected_hash = Sha256::digest(&code[..PAGE_SIZE]);
        assert_eq!(&sig[hash_start..hash_start + 32], expected_hash.as_slice());

        // Verify second (partial) page hash
        let expected_hash2 = Sha256::digest(&code[PAGE_SIZE..]);
        assert_eq!(
            &sig[hash_start + 32..hash_start + 64],
            expected_hash2.as_slice()
        );
    }

    #[test]
    fn test_sign_macho_succeeds() {
        let mut data = build_test_macho(4);
        assert!(sign_macho(&mut data, "test-binary"));

        // Verify the signature was written at the correct offset
        let sig_offset = 4 * PAGE_SIZE;
        assert_eq!(
            u32::from_be_bytes(
                data[sig_offset..sig_offset + 4].try_into().unwrap()
            ),
            CSMAGIC_EMBEDDED_SIGNATURE
        );
    }

    #[test]
    fn test_sign_macho_updates_datasize() {
        let mut data = build_test_macho(2);

        // Record original datasize
        let cs_cmd_offset = 32 + 72;
        let original_datasize = read_u32_le(&data, cs_cmd_offset + 12);

        assert!(sign_macho(&mut data, "lune"));

        // The datasize should now match the actual signature size
        let new_datasize = read_u32_le(&data, cs_cmd_offset + 12);
        let expected_size = signature_size(2 * PAGE_SIZE as u32, b"lune");
        assert_eq!(new_datasize, expected_size as u32);
        assert_ne!(new_datasize, original_datasize);
    }

    #[test]
    fn test_sign_macho_invalid_binary() {
        let mut data = vec![0u8; 100];
        assert!(!sign_macho(&mut data, "test"));
    }

    #[test]
    fn test_sign_macho_preserves_data_after_signature() {
        let mut data = build_test_macho(2);
        // Append extra data after the binary (simulating lune build metadata)
        let metadata = b"cr3sc3nt";
        data.extend_from_slice(metadata);
        assert!(sign_macho(&mut data, "lune"));

        // The metadata at the end should still be intact
        let end = data.len();
        assert_eq!(&data[end - metadata.len()..], metadata);
    }

    #[test]
    fn test_sign_macho_idempotent() {
        let mut data = build_test_macho(3);

        assert!(sign_macho(&mut data, "lune"));
        let first_sig = data.clone();

        assert!(sign_macho(&mut data, "lune"));
        // Signing twice should produce the same result since the code
        // content hasn't changed (signature is outside codeLimit)
        assert_eq!(data, first_sig);
    }

    /// Sign a real Mach-O binary with appended metadata (simulating lune
    /// build) and verify the signature is correctly structured using
    /// `codesign -d`. Note: `codesign -v` strict validation always rejects
    /// binaries with data appended after __LINKEDIT — even when signed by
    /// Apple's own `codesign` tool. The kernel's signature check is less
    /// strict and accepts them.
    #[test]
    #[cfg(target_os = "macos")]
    fn test_sign_real_binary_with_appended_data() {
        use std::process::Command;

        let lune_path = std::env::current_exe().unwrap();
        let mut binary = std::fs::read(&lune_path).unwrap();

        // Append metadata to simulate lune build patching
        binary.extend_from_slice(b"test-metadata-payload");
        binary.extend_from_slice(&20u64.to_be_bytes());
        binary.extend_from_slice(b"cr3sc3nt");

        assert!(sign_macho(&mut binary, "lune-test"));

        let tmp = std::env::temp_dir().join("lune-codesign-test");
        std::fs::write(&tmp, &binary).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        // Use codesign -d to inspect the signature (not -v which does
        // strict validation)
        let output = Command::new("codesign")
            .args(["-d", "--verbose=2"])
            .arg(&tmp)
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr);
        std::fs::remove_file(&tmp).ok();

        assert!(
            output.status.success(),
            "codesign -d failed: {stderr}"
        );
        assert!(
            stderr.contains("adhoc"),
            "expected ad-hoc signature, got: {stderr}"
        );
        assert!(
            stderr.contains("Identifier=lune-test"),
            "expected identifier 'lune-test', got: {stderr}"
        );
        assert!(
            stderr.contains("flags=0x20002"),
            "expected adhoc+linker-signed flags, got: {stderr}"
        );
    }

    /// Sign a real binary without appended data — this should pass both
    /// codesign -d (inspection) and codesign -v (validation).
    #[test]
    #[cfg(target_os = "macos")]
    fn test_sign_real_binary_validates() {
        use std::process::Command;

        let lune_path = std::env::current_exe().unwrap();
        let mut binary = std::fs::read(&lune_path).unwrap();

        assert!(sign_macho(&mut binary, "lune-idem"));
        let first = binary.clone();

        assert!(sign_macho(&mut binary, "lune-idem"));
        assert_eq!(binary, first, "re-signing changed the binary");

        let tmp = std::env::temp_dir().join("lune-codesign-validate-test");
        std::fs::write(&tmp, &binary).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        let output = Command::new("codesign")
            .args(["-v", "--verbose=2"])
            .arg(&tmp)
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr);
        std::fs::remove_file(&tmp).ok();

        assert!(
            output.status.success(),
            "codesign verification failed: {stderr}"
        );
    }

    #[test]
    fn test_not_main_binary_flag() {
        let code = vec![0u8; PAGE_SIZE];
        let sig = build_signature(&code, b"lib", 0, PAGE_SIZE as u64, false);

        let cd_offset = SUPER_BLOB_SIZE + BLOB_INDEX_SIZE;
        let exec_seg_flags = u64::from_be_bytes(
            sig[cd_offset + 80..cd_offset + 88].try_into().unwrap(),
        );
        assert_eq!(exec_seg_flags, 0); // Not a main binary
    }
}
