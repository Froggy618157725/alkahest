use std::{fmt::Display, fs::File, io::Write, sync::Arc};

use destiny_pkg::{PackageManager, PackageVersion, TagHash, TagHash64};
use eframe::epaint::mutex::RwLock;
use itertools::Itertools;
use log::{error, info, warn};
use nohash_hasher::{IntMap, IntSet};
use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};

use crate::packages::package_manager;

pub type TagCache = IntMap<TagHash, ScanResult>;

// Shareable read-only context
pub struct ScannerContext {
    pub valid_file_hashes: IntSet<TagHash>,
    pub valid_file_hashes64: IntSet<TagHash64>,
    pub known_string_hashes: IntSet<u32>,
}

#[derive(Default, Clone, serde::Deserialize, serde::Serialize)]
pub struct ScanResult {
    pub file_hashes: Vec<ScannedHash<TagHash>>,
    pub file_hashes64: Vec<ScannedHash<TagHash64>>,
    pub string_hashes: Vec<ScannedHash<u32>>,

    /// References from other files
    pub references: Vec<TagHash>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct ScannedHash<T: Sized> {
    pub offset: u64,
    pub hash: T,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct ScannedArray {
    pub offset: u64,
    pub count: usize,
    pub class: u32,
}

pub fn scan_file(context: &ScannerContext, data: &[u8]) -> ScanResult {
    let mut r = ScanResult::default();

    for (i, v) in data.chunks_exact(4).enumerate() {
        let m: [u8; 4] = v.try_into().unwrap();
        let value = u32::from_le_bytes(m);

        let offset = (i * 4) as u64;
        let hash = TagHash(value);
        if hash.is_pkg_file() && context.valid_file_hashes.contains(&hash) {
            r.file_hashes.push(ScannedHash { offset, hash });
        }

        // if hash.is_valid() && !hash.is_pkg_file() {
        //     r.classes.push(ScannedHash {
        //         offset,
        //         hash: value,
        //     });
        // }

        if value == 0x811c9dc5 || context.known_string_hashes.contains(&value) {
            r.string_hashes.push(ScannedHash {
                offset,
                hash: value,
            });
        }
    }

    for (i, v) in data.chunks_exact(8).enumerate() {
        let m: [u8; 8] = v.try_into().unwrap();
        let value = u64::from_le_bytes(m);

        let offset = (i * 8) as u64;
        let hash = TagHash64(value);
        if context.valid_file_hashes64.contains(&hash) {
            r.file_hashes64.push(ScannedHash { offset, hash });
        }
    }

    // let mut cur = Cursor::new(data);
    // for c in &r.classes {
    //     if c.hash == 0x80809fb8 {
    //         cur.seek(SeekFrom::Start(c.offset + 4)).unwrap();

    //         let mut count_bytes = [0; 8];
    //         cur.read_exact(&mut count_bytes).unwrap();
    //         let mut class_bytes = [0; 4];
    //         cur.read_exact(&mut class_bytes).unwrap();

    //         r.arrays.push(ScannedArray {
    //             offset: c.offset + 4,
    //             count: u64::from_le_bytes(count_bytes) as usize,
    //             class: u32::from_le_bytes(class_bytes),
    //         });
    //     }
    // }

    r
}

pub fn create_scanner_context(package_manager: &PackageManager) -> anyhow::Result<ScannerContext> {
    info!("Creating scanner context");

    Ok(ScannerContext {
        valid_file_hashes: package_manager
            .package_entry_index
            .iter()
            .flat_map(|(pkg_id, entries)| {
                entries
                    .iter()
                    .enumerate()
                    .map(|(entry_id, _)| TagHash::new(*pkg_id, entry_id as _))
                    .collect_vec()
            })
            .collect(),
        valid_file_hashes64: package_manager
            .hash64_table
            .keys()
            .map(|&v| TagHash64(v))
            .collect(),
        // TODO
        known_string_hashes: Default::default(),
    })
}

#[derive(Copy, Clone)]
pub enum ScanStatus {
    None,
    CreatingScanner,
    Scanning {
        current_package: usize,
        total_packages: usize,
    },
    TransformGathering,
    TransformApplying,
    WritingCache,
    LoadingCache,
}

impl Display for ScanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanStatus::None => Ok(()),
            ScanStatus::CreatingScanner => f.write_str("Creating scanner"),
            ScanStatus::Scanning {
                current_package,
                total_packages,
            } => f.write_fmt(format_args!(
                "Creating new cache {}/{}",
                current_package + 1,
                total_packages
            )),
            ScanStatus::TransformGathering => {
                f.write_str("Transforming cache (gathering references)")
            }
            ScanStatus::TransformApplying => {
                f.write_str("Transforming cache (applying references)")
            }
            ScanStatus::WritingCache => f.write_str("Writing cache"),
            ScanStatus::LoadingCache => f.write_str("Loading cache"),
        }
    }
}

lazy_static::lazy_static! {
    static ref SCANNER_PROGRESS: RwLock<ScanStatus> = RwLock::new(ScanStatus::None);
}

/// Returns Some((current_package, total_packages)) if there's a scan in progress
pub fn scanner_progress() -> ScanStatus {
    *SCANNER_PROGRESS.read()
}

pub fn load_tag_cache() -> TagCache {
    if let Ok(cache_file) = File::open("cache.bin") {
        info!("Existing cache file found, loading");
        *SCANNER_PROGRESS.write() = ScanStatus::LoadingCache;

        match zstd::Decoder::new(cache_file) {
            Ok(zstd_decoder) => {
                if let Ok(cache) = bincode::deserialize_from::<_, TagCache>(zstd_decoder) {
                    *SCANNER_PROGRESS.write() = ScanStatus::None;
                    return cache;
                } else {
                    warn!("Cache file is invalid, creating a new one");
                }
            }
            Err(e) => error!("Cache file is invalid: {e}"),
        }
    }

    *SCANNER_PROGRESS.write() = ScanStatus::CreatingScanner;
    let scanner_context = Arc::new(
        create_scanner_context(&package_manager()).expect("Failed to create scanner context"),
    );

    let all_pkgs = package_manager()
        .package_paths
        .values()
        .cloned()
        .collect_vec();

    let package_count = all_pkgs.len();
    let cache: IntMap<u32, ScanResult> = all_pkgs
        .par_iter()
        .map_with(scanner_context, |context, path| {
            let current_package = {
                let mut p = SCANNER_PROGRESS.write();
                let current_package = if let ScanStatus::Scanning {
                    current_package, ..
                } = *p
                {
                    current_package
                } else {
                    0
                };

                *p = ScanStatus::Scanning {
                    current_package: current_package + 1,
                    total_packages: package_count,
                };

                current_package
            };
            info!("Opening pkg {path} ({}/{package_count})", current_package);
            let pkg = PackageVersion::Destiny2Lightfall.open(path).unwrap();

            let mut all_tags = pkg
                .get_all_by_type(8, None)
                .iter()
                .chain(pkg.get_all_by_type(16, None).iter())
                .cloned()
                .collect_vec();

            // Sort tags by entry index to optimize sequential block reads
            all_tags.sort_by_key(|v| v.0);

            let mut results = IntMap::default();
            for (t, _) in all_tags {
                let data = match pkg.read_entry(t) {
                    Ok(d) => d,
                    Err(e) => {
                        error!("Failed to read entry {path}:{t}: {e}");
                        continue;
                    }
                };

                let hash = TagHash::new(pkg.pkg_id(), t as u16);
                let scan_result = scan_file(context, &data);
                results.insert(hash.0, scan_result);
            }

            results
        })
        .flatten()
        .collect();

    let cache = transform_tag_cache(cache);

    *SCANNER_PROGRESS.write() = ScanStatus::WritingCache;
    info!("Serializing tag cache...");
    let cache_bincode = bincode::serialize(&cache).unwrap();
    info!("Compressing tag cache...");
    let mut writer = zstd::Encoder::new(File::create("cache.bin").unwrap(), 5).unwrap();
    writer.write_all(&cache_bincode).unwrap();
    writer.finish().unwrap();
    *SCANNER_PROGRESS.write() = ScanStatus::None;

    cache
}

/// Transforms the tag cache to include reference lookup tables
fn transform_tag_cache(cache: IntMap<u32, ScanResult>) -> TagCache {
    info!("Transforming tag cache...");

    let mut new_cache: TagCache = Default::default();

    *SCANNER_PROGRESS.write() = ScanStatus::TransformGathering;
    info!("\t- Gathering references");
    let mut direct_reference_cache: IntMap<u32, Vec<TagHash>> = Default::default();
    for (k2, v2) in &cache {
        for t32 in &v2.file_hashes {
            match direct_reference_cache.entry(t32.hash.0) {
                std::collections::hash_map::Entry::Occupied(mut o) => {
                    o.get_mut().push(TagHash(*k2));
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(vec![TagHash(*k2)]);
                }
            }
        }

        for t64 in &v2.file_hashes64 {
            if let Some(t32) = package_manager().hash64_table.get(&t64.hash.0) {
                match direct_reference_cache.entry(t32.hash32.0) {
                    std::collections::hash_map::Entry::Occupied(mut o) => {
                        o.get_mut().push(TagHash(*k2));
                    }
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(vec![TagHash(*k2)]);
                    }
                }
            }
        }
    }

    *SCANNER_PROGRESS.write() = ScanStatus::TransformApplying;
    info!("\t- Applying references");
    for (k, v) in &cache {
        let mut scan = v.clone();

        if let Some(refs) = direct_reference_cache.get(k) {
            scan.references = refs.clone();
        }

        new_cache.insert(TagHash(*k), scan);
    }

    new_cache
}