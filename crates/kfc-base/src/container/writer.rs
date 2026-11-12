use std::{
    borrow::Borrow,
    fs::{File, OpenOptions},
    io::{self, BufWriter, Cursor, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use crate::{
    container::header::ResourceChunkInfo,
    guid::{ContentHash, ResourceId},
    io::{WriteExt, WriteSeekExt},
    reflection::TypeRegistry,
};

use super::{
    header::{ContainerInfo, ContentEntry, ResourceEntry},
    KFCFile, KFCReadError, KFCWriteError, StaticMapBuilder,
};

#[path = "generated_dat_reservation.rs"]
mod generated_dat_reservation;

const RESOURCE_ALIGNMENT: u64 = 16;
const CONTENT_ALIGNMENT: u64 = 4096;
const RESOURCE_CHUNK_ALIGNMENT: u64 = 4096;
const RESOURCE_CHUNK_SIZE: u64 = 8 * 1024 * 1024; // 8 MiB
const GENERATED_RESOURCE_FIELD_MAX: u64 = u32::MAX as u64;

fn generated_resource_overflow() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "generated resource value exceeds KFC u32 field limits",
    )
}

fn check_generated_resource_field(value: u64) -> io::Result<()> {
    if value <= GENERATED_RESOURCE_FIELD_MAX {
        Ok(())
    } else {
        Err(generated_resource_overflow())
    }
}

fn checked_generated_resource_add(start: u64, length: u64) -> io::Result<u64> {
    check_generated_resource_field(start)?;
    check_generated_resource_field(length)?;
    let end = start
        .checked_add(length)
        .ok_or_else(generated_resource_overflow)?;
    check_generated_resource_field(end)?;
    Ok(end)
}

fn checked_generated_resource_align(value: u64, alignment: u64) -> io::Result<u64> {
    if alignment == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "generated resource alignment must be nonzero",
        ));
    }
    check_generated_resource_field(value)?;
    let padding = (alignment - (value % alignment)) % alignment;
    let aligned = value
        .checked_add(padding)
        .ok_or_else(generated_resource_overflow)?;
    check_generated_resource_field(aligned)?;
    Ok(aligned)
}

fn check_generated_resource_entry(offset: u64, size: u64) -> io::Result<u64> {
    check_generated_resource_field(offset)?;
    check_generated_resource_field(size)?;
    checked_generated_resource_add(offset, size)
}

fn check_generated_resource_chunk(chunk: &ResourceChunkInfo) -> io::Result<()> {
    check_generated_resource_field(chunk.offset)?;
    check_generated_resource_field(chunk.size)?;
    check_generated_resource_field(chunk.compressed_size)?;
    check_generated_resource_field(chunk.uncompressed_offset)?;
    check_generated_resource_field(chunk.uncompressed_size)?;
    checked_generated_resource_add(chunk.offset, chunk.size)?;
    checked_generated_resource_add(chunk.uncompressed_offset, chunk.uncompressed_size)?;
    Ok(())
}

pub struct KFCWriter<F, T> {
    type_registry: T,

    path: PathBuf,
    file_name: String,

    file: File,

    chunk_writer: Cursor<Vec<u8>>,
    resource_writer: BufWriter<File>,
    uncompressed_offset: u64,
    uncompressed_chunk_offset: u64,
    resource_chunks: Vec<ResourceChunkInfo>,

    container_writers: Vec<ContainerWriter>,

    game_version: String,
    resources: StaticMapBuilder<ResourceId, ResourceEntry>,
    contents: StaticMapBuilder<ContentHash, ContentEntry>,

    incremental_data: Option<IncrementalData<F>>,
    generated_prefix: Option<GeneratedPrefix>,
    generated_reservation: Option<generated_dat_reservation::Reservation>,
    options: KFCWriteOptions,
}

struct IncrementalData<F> {
    reference_file: F,
}

struct GeneratedPrefix {
    source: File,
    length: u64,
}

#[derive(Debug)]
pub struct KFCWriteOptions {
    /// Preserve all bytes in an incremental resource file, including unreferenced
    /// trailing data. Used when generating an output from composed assets.
    pub append_after_resource_file: bool,
    /// The extension to use for kfc files.
    pub kfc_extension: String,
    /// The extension to use for kfc resource files.
    pub resource_extension: String,
    /// The extension to use for dat files.
    pub dat_extension: String,
    /// If true, the writer will overwrite existing data in the .dat files,
    /// instead of causing an error.
    /// **NOTE:** For incremental writes, this will only affect non-default .dat files.
    pub overwrite_containers: bool,
    // TEST: does the game use a i32, u32 or an artifical limit for the dat max size?
    /// The maximum size of a single .dat file before a new one is created.
    pub max_container_size: u32,
    /// Whether to truncate existing .dat files or not.
    /// This is used to remove any leftover data from previous writes.
    /// **NOTE:** For incremental writes, this will only affect non-default .dat files.
    pub truncate_containers: bool,
}

impl Default for KFCWriteOptions {
    fn default() -> Self {
        Self {
            append_after_resource_file: false,
            kfc_extension: "kfc".to_string(),
            resource_extension: "kfc_resources".to_string(),
            dat_extension: "dat".to_string(),
            overwrite_containers: false,
            max_container_size: 1024 * 1024 * 1024, // 1 GiB
            truncate_containers: false,
        }
    }
}

impl<F, T> KFCWriter<F, T>
where
    F: Borrow<KFCFile>,
    T: Borrow<TypeRegistry>,
{
    #[inline]
    pub fn new(
        path: impl AsRef<Path>,
        file_name: impl AsRef<str>,
        type_registry: T,
        game_version: impl AsRef<str>,
    ) -> Result<Self, KFCReadError> {
        Self::new_with_options(
            path,
            file_name,
            type_registry,
            game_version,
            KFCWriteOptions::default(),
        )
    }

    pub fn new_with_options(
        path: impl AsRef<Path>,
        file_name: impl AsRef<str>,
        type_registry: T,
        game_version: impl AsRef<str>,
        options: KFCWriteOptions,
    ) -> Result<Self, KFCReadError> {
        let kfc_path =
            path.as_ref()
                .join(format!("{}.{}", file_name.as_ref(), options.kfc_extension));
        let file = File::options().write(true).read(true).open(&kfc_path)?;

        let resources_path = path.as_ref().join(format!(
            "{}.{}",
            file_name.as_ref(),
            options.resource_extension
        ));
        let resource_file = File::options().write(true).open(&resources_path)?;
        let resource_writer = BufWriter::new(resource_file);

        Ok(Self {
            type_registry,

            path: path.as_ref().to_path_buf(),
            file_name: file_name.as_ref().to_string(),

            file,

            chunk_writer: Cursor::new(Vec::new()),
            resource_writer,
            uncompressed_offset: 0,
            uncompressed_chunk_offset: 0,
            resource_chunks: Vec::new(),

            container_writers: Vec::new(),

            game_version: game_version.as_ref().to_string(),
            resources: StaticMapBuilder::default(),
            contents: StaticMapBuilder::default(),

            incremental_data: None,
            generated_prefix: None,
            generated_reservation: None,
            options,
        })
    }

    #[inline]
    pub fn new_incremental(
        path: impl AsRef<Path>,
        file_name: impl AsRef<str>,
        reference_file: F,
        type_registry: T,
    ) -> Result<Self, KFCReadError> {
        Self::new_incremental_with_options(
            path,
            file_name,
            reference_file,
            type_registry,
            KFCWriteOptions::default(),
        )
    }

    pub fn new_incremental_with_options(
        path: impl AsRef<Path>,
        file_name: impl AsRef<str>,
        reference_file: F,
        type_registry: T,
        options: KFCWriteOptions,
    ) -> Result<Self, KFCReadError> {
        let kfc_path =
            path.as_ref()
                .join(format!("{}.{}", file_name.as_ref(), options.kfc_extension));
        let current_file = KFCFile::from_path(&kfc_path, true)?;

        let file = reference_file.borrow();

        let resources = file.resources().as_builder();
        let contents = file.contents().as_builder();

        drop(current_file);

        let file = File::options().write(true).read(true).open(&kfc_path)?;

        let resources_path = path.as_ref().join(format!(
            "{}.{}",
            file_name.as_ref(),
            options.resource_extension
        ));
        let resource_file = File::options().write(true).open(&resources_path)?;
        let mut resource_writer = BufWriter::new(resource_file);

        let previous_resource_size = reference_file
            .borrow()
            .resource_chunks()
            .iter()
            .map(|chunk| chunk.offset + chunk.size)
            .max()
            .unwrap_or(0);

        let previous_uncompressed_size = reference_file
            .borrow()
            .resource_chunks()
            .iter()
            .map(|chunk| chunk.uncompressed_offset + chunk.uncompressed_size)
            .max()
            .unwrap_or(0);

        if options.append_after_resource_file {
            let end = resource_writer.seek(SeekFrom::End(0))?;
            if end < previous_resource_size {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "resource file is shorter than the reference index",
                )
                .into());
            }
            resource_writer.align(RESOURCE_CHUNK_ALIGNMENT as usize)?;
        } else {
            resource_writer.seek(SeekFrom::Start(previous_resource_size))?;
        }

        Ok(Self {
            type_registry,

            path: path.as_ref().into(),
            file_name: file_name.as_ref().to_string(),

            file,
            chunk_writer: Cursor::new(Vec::new()),
            resource_writer,
            uncompressed_offset: previous_uncompressed_size,
            uncompressed_chunk_offset: previous_uncompressed_size,
            resource_chunks: Vec::new(),

            container_writers: Vec::new(),

            game_version: reference_file.borrow().game_version().to_string(),
            resources,
            contents,

            incremental_data: Some(IncrementalData { reference_file }),
            generated_prefix: None,
            generated_reservation: None,
            options,
        })
    }

    /// Create a generated KFC replacement in a separate output directory,
    /// using the source directory as a read-only baseline. Existing source DAT
    /// slots remain in the new index, and generated DATs follow occupied slots.
    pub fn new_generated_incremental(
        source: impl AsRef<Path>,
        output: impl AsRef<Path>,
        file_name: impl AsRef<str>,
        reference_file: F,
        type_registry: T,
    ) -> Result<Self, KFCReadError> {
        let source = source.as_ref();
        let output = output.as_ref();
        let file_name = file_name.as_ref();
        let options = KFCWriteOptions {
            append_after_resource_file: true,
            ..KFCWriteOptions::default()
        };
        let reference = reference_file.borrow();

        // Inspect every physical source DAT before creating output files.
        let generated_reservation = generated_dat_reservation::reserve(
            source,
            file_name,
            &options.dat_extension,
            reference.containers(),
        )?;

        let generated_prefix =
            File::open(source.join(format!("{}.{}", file_name, options.resource_extension)))?;
        let generated_prefix_length = generated_prefix.metadata()?.len();
        let generated_suffix_offset =
            checked_generated_resource_align(generated_prefix_length, RESOURCE_CHUNK_ALIGNMENT)?;
        let previous_resource_size =
            reference
                .resource_chunks()
                .iter()
                .try_fold(0u64, |max_end, chunk| {
                    let end = checked_generated_resource_add(chunk.offset, chunk.size)?;
                    Ok::<u64, io::Error>(max_end.max(end))
                })?;
        if generated_prefix_length < previous_resource_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "resource file is shorter than the reference index",
            )
            .into());
        }

        let previous_uncompressed_size =
            reference
                .resource_chunks()
                .iter()
                .try_fold(0u64, |max_end, chunk| {
                    let end = checked_generated_resource_add(
                        chunk.uncompressed_offset,
                        chunk.uncompressed_size,
                    )?;
                    Ok::<u64, io::Error>(max_end.max(end))
                })?;
        let resources = reference.resources().as_builder();
        let contents = reference.contents().as_builder();

        let kfc_path = output.join(format!("{}.{}", file_name, options.kfc_extension));
        let resources_path = output.join(format!("{}.{}", file_name, options.resource_extension));
        if kfc_path.exists() || resources_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "generated KFC output already exists",
            )
            .into());
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&kfc_path)?;
        let resource_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&resources_path)?;
        let mut resource_writer = BufWriter::new(resource_file);

        // Keep the generated suffix beyond the complete source file, including
        // bytes not referenced by the source resource index.
        resource_writer.seek(SeekFrom::Start(generated_suffix_offset))?;

        Ok(Self {
            type_registry,
            path: output.to_path_buf(),
            file_name: file_name.to_string(),
            file,
            chunk_writer: Cursor::new(Vec::new()),
            resource_writer,
            uncompressed_offset: previous_uncompressed_size,
            uncompressed_chunk_offset: previous_uncompressed_size,
            resource_chunks: Vec::new(),
            container_writers: Vec::new(),
            game_version: reference.game_version().to_string(),
            resources,
            contents,
            incremental_data: Some(IncrementalData { reference_file }),
            generated_prefix: Some(GeneratedPrefix {
                source: generated_prefix,
                length: generated_prefix_length,
            }),
            generated_reservation: Some(generated_reservation),
            options,
        })
    }

    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[inline]
    pub fn type_registry(&self) -> &TypeRegistry {
        self.type_registry.borrow()
    }

    #[inline]
    pub fn reference_file(&self) -> Option<&KFCFile> {
        self.incremental_data
            .as_ref()
            .map(|data| data.reference_file.borrow())
    }

    pub fn write_resource(&mut self, guid: &ResourceId, mut bytes: &[u8]) -> std::io::Result<()> {
        let previous_offset = self.uncompressed_offset;
        let generated_aligned_end = if self.generated_reservation.is_some() {
            let size = u64::try_from(bytes.len()).map_err(|_| generated_resource_overflow())?;
            Some(checked_generated_resource_align(
                check_generated_resource_entry(previous_offset, size)?,
                RESOURCE_ALIGNMENT,
            )?)
        } else {
            None
        };

        self.resources.insert(
            *guid,
            ResourceEntry {
                offset: previous_offset,
                size: bytes.len() as u64,
            },
        );

        if let Some(aligned_end) = generated_aligned_end {
            self.uncompressed_offset = aligned_end;
        } else {
            self.uncompressed_offset += bytes.len() as u64;
            self.uncompressed_offset += (RESOURCE_ALIGNMENT
                - (self.uncompressed_offset % RESOURCE_ALIGNMENT))
                % RESOURCE_ALIGNMENT;
        }

        let mut aligned_size = self.uncompressed_offset - previous_offset;

        // write resource data or compress it if needed

        while aligned_size + self.chunk_writer.position() >= RESOURCE_CHUNK_SIZE {
            let available_size = RESOURCE_CHUNK_SIZE - self.chunk_writer.position();
            let split_index = (available_size as usize).min(bytes.len());

            let (chunk_bytes, remaining_bytes) = bytes.split_at(split_index);

            self.chunk_writer.write_all(chunk_bytes)?;
            self.chunk_writer.align(RESOURCE_CHUNK_SIZE as usize)?;

            // compress and submit chunk

            self.submit_resource_data()?;

            // continue with remaining bytes

            bytes = remaining_bytes;
            aligned_size -= available_size;
        }

        if aligned_size > 0 {
            self.chunk_writer.write_all(bytes)?;

            let remaining = aligned_size - bytes.len() as u64;

            if remaining > 0 {
                self.chunk_writer.padding(remaining)?;
            }
        }

        Ok(())
    }

    fn submit_resource_data(&mut self) -> std::io::Result<()> {
        let generated_output = self.generated_reservation.is_some();
        let offset = self.resource_writer.stream_position()?;
        let uncompressed_offset = self.uncompressed_chunk_offset;
        let chunk_data = self.chunk_writer.get_ref();
        let uncompressed_size = if generated_output {
            u64::try_from(chunk_data.len()).map_err(|_| generated_resource_overflow())?
        } else {
            chunk_data.len() as u64
        };
        let generated_uncompressed_end = if generated_output {
            check_generated_resource_field(offset)?;
            Some(checked_generated_resource_add(
                uncompressed_offset,
                uncompressed_size,
            )?)
        } else {
            None
        };

        zstd::stream::copy_encode(&mut &chunk_data[..], &mut self.resource_writer, 0)?;

        let compressed_end = self.resource_writer.stream_position()?;
        let compressed_size = if generated_output {
            compressed_end
                .checked_sub(offset)
                .ok_or_else(generated_resource_overflow)?
        } else {
            compressed_end - offset
        };
        let expected_generated_end = if generated_output {
            let unaligned_end = checked_generated_resource_add(offset, compressed_size)?;
            Some(checked_generated_resource_align(
                unaligned_end,
                RESOURCE_CHUNK_ALIGNMENT,
            )?)
        } else {
            None
        };

        self.resource_writer
            .align(RESOURCE_CHUNK_ALIGNMENT as usize)?;

        let physical_end = self.resource_writer.stream_position()?;
        let size = if generated_output {
            physical_end
                .checked_sub(offset)
                .ok_or_else(generated_resource_overflow)?
        } else {
            physical_end - offset
        };

        let chunk = ResourceChunkInfo {
            offset,
            size,
            compressed_size,
            uncompressed_offset,
            uncompressed_size,
        };
        if generated_output {
            check_generated_resource_chunk(&chunk)?;
            if expected_generated_end != Some(physical_end) {
                return Err(generated_resource_overflow());
            }
        }
        self.resource_chunks.push(chunk);

        if let Some(end) = generated_uncompressed_end {
            self.uncompressed_chunk_offset = end;
        } else {
            self.uncompressed_chunk_offset += uncompressed_size;
        }

        // reset chunk writer

        self.chunk_writer.set_position(0);
        self.chunk_writer.get_mut().clear();

        Ok(())
    }

    pub fn write_content(&mut self, guid: &ContentHash, data: &[u8]) -> std::io::Result<()> {
        if guid.size() != data.len() as u32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Content size mismatch",
            ));
        }

        if self.contents.contains_key(guid) {
            // if the content already exists, we don't need to write it again since they are unique
            return Ok(());
        }

        let container_writer = self.get_container_writer()?;
        let index = container_writer.index;

        let writer = container_writer.aquire()?;
        let offset = writer.stream_position()?;

        writer.write_all(data)?;
        writer.align(CONTENT_ALIGNMENT as usize)?;

        self.contents
            .insert(*guid, ContentEntry::new(offset, 0, index));

        Ok(())
    }

    pub fn finalize(mut self) -> Result<(), KFCWriteError> {
        self.submit_resource_data()?;

        if let Some(mut prefix) = self.generated_prefix.take() {
            stream_generated_prefix(&mut prefix.source, &mut self.resource_writer, prefix.length)?;
        }

        // prepare container infos

        let generated_output = self.generated_reservation.is_some();
        let mut containers = if let Some(reservation) = self.generated_reservation.take() {
            for index in reservation.missing_padding {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(self.dat_path(index))?;
            }
            reservation.containers
        } else {
            self.incremental_data
                .as_ref()
                .map(|data| data.reference_file.borrow().containers().to_vec())
                .unwrap_or_default()
        };

        containers.reserve_exact(self.container_writers.len());

        self.container_writers.sort_by_key(|w| w.index);

        for container_writer in &mut self.container_writers {
            container_writer.flush()?;

            containers.push(ContainerInfo {
                size: container_writer.position()?,
                count: container_writer.count,
            });
        }

        // make sure to have a power of two container files

        let required_container_count = containers.len().next_power_of_two();

        while containers.len() < required_container_count {
            let path = self.dat_path(containers.len());

            if generated_output {
                OpenOptions::new().write(true).create_new(true).open(path)?;
            } else if !path.exists() || self.options.truncate_containers {
                File::create(path)?;
            }

            containers.push(ContainerInfo { size: 0, count: 0 });
        }

        // prepare resource chunks

        let mut chunks = self
            .incremental_data
            .as_ref()
            .map(|data| data.reference_file.borrow().resource_chunks().to_vec())
            .unwrap_or_default();

        chunks.extend(self.resource_chunks.iter().cloned());
        let resources = self.resources.build();
        if generated_output {
            check_generated_resource_field(
                u64::try_from(resources.len()).map_err(|_| generated_resource_overflow())?,
            )?;
            check_generated_resource_field(
                u64::try_from(chunks.len()).map_err(|_| generated_resource_overflow())?,
            )?;
            let total_compressed_size = chunks.iter().try_fold(0u64, |total, chunk| {
                checked_generated_resource_add(total, chunk.compressed_size)
            })?;
            let total_uncompressed_size = chunks.iter().try_fold(0u64, |total, chunk| {
                checked_generated_resource_add(total, chunk.uncompressed_size)
            })?;
            check_generated_resource_field(total_compressed_size)?;
            check_generated_resource_field(total_uncompressed_size)?;
        }

        // header construction

        let mut file_writer = BufWriter::new(&mut self.file);
        let mut file = KFCFile::default();

        file.set_game_version(self.game_version);
        file.set_resources(
            resources,
            self.type_registry.borrow(),
            self.incremental_data
                .as_ref()
                .map(|data| data.reference_file.borrow().resource_bundles()),
        )?;
        file.set_contents(self.contents.build());
        file.set_containers(containers);
        file.set_resource_chunks(chunks);

        file.write(&mut file_writer)?;

        Ok(())
    }

    pub fn dat_path(&self, index: usize) -> PathBuf {
        // Format: FILE_NAME_{INDEX}.dat where INDEX is 3 digits with leading zeros
        let name = format!(
            "{}_{:03}.{}",
            self.file_name, index, self.options.dat_extension
        );
        self.path.join(name)
    }

    fn get_container_writer(&mut self) -> std::io::Result<&mut ContainerWriter> {
        if !self.container_writers.is_empty() {
            let writer = self.container_writers.last_mut().unwrap();
            let pos = writer.position()?;

            if pos < self.options.max_container_size as u64 {
                return Ok(self.container_writers.last_mut().unwrap());
            }
        }

        // create a new container writer

        let generated_output = self.generated_reservation.is_some();
        let base_index = if let Some(reservation) = self.generated_reservation.as_ref() {
            reservation.containers.len()
        } else {
            self.incremental_data
                .as_ref()
                .map(|data| data.reference_file.borrow().containers().len())
                .unwrap_or(0)
        };
        let next_index = base_index + self.container_writers.len();
        if generated_output && next_index > u16::MAX as usize {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "generated container index exceeds format",
            ));
        }
        let path = self.dat_path(next_index);

        if !generated_output && !self.options.overwrite_containers && path.exists() {
            // make sure we don't accidentally overwrite an existing file
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Data file already exists: {}", path.display()),
            ));
        }

        let file = if generated_output {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?
        } else {
            File::create(&path)?
        };
        let writer = BufWriter::new(file);
        let writer = ContainerWriter::new(next_index, writer);

        self.container_writers.push(writer);

        Ok(self.container_writers.last_mut().unwrap())
    }
}

fn stream_generated_prefix(
    source: &mut impl Read,
    output: &mut (impl Write + Seek),
    length: u64,
) -> std::io::Result<()> {
    let end = output.stream_position()?;
    output.seek(SeekFrom::Start(0))?;
    let copied = io::copy(&mut source.take(length), output)?;
    if copied != length {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "resource prefix changed during generated output",
        ));
    }
    output.seek(SeekFrom::Start(end))?;
    Ok(())
}

struct ContainerWriter {
    index: usize,
    count: usize,
    writer: BufWriter<File>,
}

impl ContainerWriter {
    #[inline]
    fn new(index: usize, writer: BufWriter<File>) -> Self {
        Self {
            index,
            count: 0,
            writer,
        }
    }

    #[inline]
    fn aquire(&mut self) -> std::io::Result<&mut BufWriter<File>> {
        self.count += 1;
        Ok(&mut self.writer)
    }

    #[inline]
    fn position(&mut self) -> std::io::Result<u64> {
        self.writer.stream_position()
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("kfc-{name}-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        _root: TempRoot,
        source: PathBuf,
        output: PathBuf,
        registry: TypeRegistry,
        reference: KFCFile,
        old_content: ContentHash,
    }

    impl Fixture {
        fn new() -> Self {
            let root = TempRoot::new("generated-writer");
            let source = root.path().join("source");
            let output = root.path().join("output");
            fs::create_dir(&source).unwrap();
            fs::create_dir(&output).unwrap();
            File::create(source.join("game.kfc")).unwrap();
            File::create(source.join("game.kfc_resources")).unwrap();

            let registry = TypeRegistry::default();
            let old_bytes = b"existing mapped content";
            let old_content = ContentHash::from_data(old_bytes);
            let mut writer =
                KFCWriter::<KFCFile, _>::new(&source, "game", &registry, "fixture").unwrap();
            writer.write_content(&old_content, old_bytes).unwrap();
            writer.finalize().unwrap();
            let reference = KFCFile::from_path(source.join("game.kfc"), false).unwrap();

            Self {
                _root: root,
                source,
                output,
                registry,
                reference,
                old_content,
            }
        }
    }

    fn snapshot(directory: &Path) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().into_string().unwrap(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn generated_output_reserves_unreferenced_source_dat_slots_and_preserves_maps() {
        let fixture = Fixture::new();
        fs::write(fixture.source.join("game_007.dat"), b"occupied-seven").unwrap();
        let source_before = snapshot(&fixture.source);
        let existing = fixture
            .reference
            .contents()
            .get(&fixture.old_content)
            .unwrap();
        let existing_mapping = (existing.offset, existing.flags, existing.container_index);
        let referenced_container = &fixture.reference.containers()[0];
        let referenced_container_info = (referenced_container.size, referenced_container.count);

        let bytes = b"new generated content";
        let id = ContentHash::from_data(bytes);
        let mut writer = KFCWriter::new_generated_incremental(
            &fixture.source,
            &fixture.output,
            "game",
            &fixture.reference,
            &fixture.registry,
        )
        .unwrap();
        writer.write_content(&id, bytes).unwrap();
        writer.finalize().unwrap();

        assert_eq!(snapshot(&fixture.source), source_before);
        let generated = KFCFile::from_path(fixture.output.join("game.kfc"), false).unwrap();
        assert_eq!(
            generated
                .contents()
                .get(&fixture.old_content)
                .map(|entry| { (entry.offset, entry.flags, entry.container_index) }),
            Some(existing_mapping)
        );
        assert_eq!(
            (
                generated.containers()[0].size,
                generated.containers()[0].count
            ),
            referenced_container_info
        );
        assert_eq!(generated.contents().get(&id).unwrap().container_index, 8);
        assert_eq!(
            generated.containers()[7].size,
            b"occupied-seven".len() as u64
        );
        assert_eq!(generated.containers()[7].count, 0);
        assert_eq!(generated.containers().len(), 16);

        assert!(!fixture.output.join("game_000.dat").exists());
        assert!(!fixture.output.join("game_007.dat").exists());
        for index in 1..7 {
            assert_eq!(
                fs::metadata(fixture.output.join(format!("game_{index:03}.dat")))
                    .unwrap()
                    .len(),
                0
            );
        }
        for index in 8..16 {
            assert!(fixture
                .output
                .join(format!("game_{index:03}.dat"))
                .is_file());
        }

        let mut reader = super::super::KFCReader::new(&fixture.output, "game")
            .unwrap()
            .into_cursor()
            .unwrap();
        assert_eq!(reader.read_content(&id).unwrap().unwrap(), bytes);
    }

    #[test]
    fn generated_output_streams_the_exact_resource_prefix_before_appended_chunks() {
        let fixture = Fixture::new();
        fs::write(fixture.source.join("game_007.dat"), b"occupied-seven").unwrap();
        let prefix_path = fixture.source.join("game.kfc_resources");
        fs::OpenOptions::new()
            .append(true)
            .open(&prefix_path)
            .unwrap()
            .write_all(b"unreferenced resource EOF")
            .unwrap();
        let prefix = fs::read(&prefix_path).unwrap();
        let source_before = snapshot(&fixture.source);
        let bytes = b"new generated content";
        let id = ContentHash::from_data(bytes);
        let resource_id =
            ResourceId::parse("00000001-0000-0000-0000-000000000001", 0x51ea_c0de, 0).unwrap();
        let resource_bytes = b"new generated resource bytes";
        let mut writer = KFCWriter::new_generated_incremental(
            &fixture.source,
            &fixture.output,
            "game",
            &fixture.reference,
            &fixture.registry,
        )
        .unwrap();
        writer.write_content(&id, bytes).unwrap();
        writer.write_resource(&resource_id, resource_bytes).unwrap();
        // The fixture uses an empty registry. Drop only the synthetic resource
        // map so finalization can verify chunk placement without type metadata.
        writer.resources = StaticMapBuilder::default();
        writer.finalize().unwrap();

        assert_eq!(snapshot(&fixture.source), source_before);
        let output_bytes = fs::read(fixture.output.join("game.kfc_resources")).unwrap();
        assert_eq!(&output_bytes[..prefix.len()], prefix.as_slice());

        let generated = KFCFile::from_path(fixture.output.join("game.kfc"), false).unwrap();
        let generated_chunk = generated.resource_chunks().last().unwrap();
        assert!(generated_chunk.offset >= prefix.len() as u64);
        assert_eq!(generated_chunk.offset % RESOURCE_CHUNK_ALIGNMENT, 0);
        let compressed_start = generated_chunk.offset as usize;
        let compressed_end = compressed_start + generated_chunk.compressed_size as usize;
        let decoded =
            zstd::stream::decode_all(&output_bytes[compressed_start..compressed_end]).unwrap();
        assert_eq!(&decoded[..resource_bytes.len()], resource_bytes);

        let mut reader = super::super::KFCReader::new(&fixture.output, "game")
            .unwrap()
            .into_cursor()
            .unwrap();
        assert_eq!(reader.read_content(&id).unwrap().unwrap(), bytes);
    }

    #[test]
    fn generated_writer_refuses_invalid_source_names_and_output_collisions() {
        let fixture = Fixture::new();
        for (name, is_directory) in [
            ("game_01.dat", false),
            ("game_001.dat", true),
            ("game_65536.dat", false),
        ] {
            let invalid = TempRoot::new("generated-invalid-source");
            let source = invalid.path().join("source");
            fs::create_dir(&source).unwrap();
            let path = source.join(name);
            if is_directory {
                fs::create_dir(path).unwrap();
            } else {
                fs::write(path, b"invalid source slot").unwrap();
            }
            assert!(generated_dat_reservation::reserve(&source, "game", "dat", &[]).is_err());
        }

        let kfc_path = fixture.output.join("game.kfc");
        fs::write(&kfc_path, b"existing output index").unwrap();
        let collision_bytes = fs::read(&kfc_path).unwrap();
        assert!(KFCWriter::new_generated_incremental(
            &fixture.source,
            &fixture.output,
            "game",
            &fixture.reference,
            &fixture.registry,
        )
        .is_err());
        assert_eq!(fs::read(&kfc_path).unwrap(), collision_bytes);
        assert!(!fixture.output.join("game.kfc_resources").exists());

        let resource_collision = Fixture::new();
        let resources_path = resource_collision.output.join("game.kfc_resources");
        fs::write(&resources_path, b"existing output resources").unwrap();
        let resource_collision_bytes = fs::read(&resources_path).unwrap();
        assert!(KFCWriter::new_generated_incremental(
            &resource_collision.source,
            &resource_collision.output,
            "game",
            &resource_collision.reference,
            &resource_collision.registry,
        )
        .is_err());
        assert_eq!(fs::read(&resources_path).unwrap(), resource_collision_bytes);
        assert!(!resource_collision.output.join("game.kfc").exists());

        let fixture = Fixture::new();
        fs::write(fixture.output.join("game_001.dat"), b"existing DAT output").unwrap();
        let dat_before = fs::read(fixture.output.join("game_001.dat")).unwrap();
        let mut writer = KFCWriter::new_generated_incremental(
            &fixture.source,
            &fixture.output,
            "game",
            &fixture.reference,
            &fixture.registry,
        )
        .unwrap();
        assert!(writer
            .write_content(&ContentHash::from_data(b"new content"), b"new content")
            .is_err());
        assert_eq!(
            fs::read(fixture.output.join("game_001.dat")).unwrap(),
            dat_before
        );
    }

    #[test]
    fn generated_resource_representability_checks_exact_limits_without_large_files() {
        let max_aligned_resource_eof =
            GENERATED_RESOURCE_FIELD_MAX & !(RESOURCE_CHUNK_ALIGNMENT - 1);
        assert_eq!(
            checked_generated_resource_align(max_aligned_resource_eof, RESOURCE_CHUNK_ALIGNMENT,)
                .unwrap(),
            max_aligned_resource_eof
        );
        assert!(checked_generated_resource_align(
            max_aligned_resource_eof + 1,
            RESOURCE_CHUNK_ALIGNMENT,
        )
        .is_err());
        assert!(checked_generated_resource_align(u64::MAX, RESOURCE_CHUNK_ALIGNMENT).is_err());

        assert_eq!(
            check_generated_resource_entry(GENERATED_RESOURCE_FIELD_MAX - 1, 1).unwrap(),
            GENERATED_RESOURCE_FIELD_MAX
        );
        assert!(check_generated_resource_entry(GENERATED_RESOURCE_FIELD_MAX, 1).is_err());

        let exact_chunk = ResourceChunkInfo {
            offset: max_aligned_resource_eof - RESOURCE_CHUNK_ALIGNMENT,
            size: RESOURCE_CHUNK_ALIGNMENT,
            compressed_size: RESOURCE_CHUNK_ALIGNMENT - 16,
            uncompressed_offset: GENERATED_RESOURCE_FIELD_MAX - 1,
            uncompressed_size: 1,
        };
        check_generated_resource_chunk(&exact_chunk).unwrap();

        let physical_end_overflow = ResourceChunkInfo {
            offset: max_aligned_resource_eof,
            size: RESOURCE_CHUNK_ALIGNMENT,
            ..exact_chunk.clone()
        };
        assert!(check_generated_resource_chunk(&physical_end_overflow).is_err());

        let uncompressed_end_overflow = ResourceChunkInfo {
            uncompressed_offset: GENERATED_RESOURCE_FIELD_MAX,
            uncompressed_size: 1,
            ..exact_chunk
        };
        assert!(check_generated_resource_chunk(&uncompressed_end_overflow).is_err());
    }

    #[test]
    fn generated_constructor_rejects_uncompressed_append_base_before_creating_outputs() {
        let fixture = Fixture::new();
        let mut reference = fixture.reference.clone();
        reference.set_resource_chunks(vec![ResourceChunkInfo {
            offset: 0,
            size: 0,
            compressed_size: 0,
            uncompressed_offset: GENERATED_RESOURCE_FIELD_MAX - 15,
            uncompressed_size: 32,
        }]);

        assert!(KFCWriter::new_generated_incremental(
            &fixture.source,
            &fixture.output,
            "game",
            &reference,
            &fixture.registry,
        )
        .is_err());
        assert!(fs::read_dir(&fixture.output).unwrap().next().is_none());
    }

    #[test]
    fn generated_resource_entries_accept_exact_limit_and_reject_overflow_before_mutation() {
        let fixture = Fixture::new();
        let id = ResourceId::parse("00000002-0000-0000-0000-000000000002", 7, 0).unwrap();
        let mut writer = KFCWriter::new_generated_incremental(
            &fixture.source,
            &fixture.output,
            "game",
            &fixture.reference,
            &fixture.registry,
        )
        .unwrap();
        writer.uncompressed_offset = GENERATED_RESOURCE_FIELD_MAX - 16;
        writer.write_resource(&id, b"x").unwrap();
        assert_eq!(
            writer.uncompressed_offset,
            GENERATED_RESOURCE_FIELD_MAX - 15
        );
        assert!(writer.resources.contains_key(&id));

        let fixture = Fixture::new();
        let mut writer = KFCWriter::new_generated_incremental(
            &fixture.source,
            &fixture.output,
            "game",
            &fixture.reference,
            &fixture.registry,
        )
        .unwrap();
        writer.uncompressed_offset = GENERATED_RESOURCE_FIELD_MAX - 15;
        assert!(writer.write_resource(&id, b"x").is_err());
        assert_eq!(
            writer.uncompressed_offset,
            GENERATED_RESOURCE_FIELD_MAX - 15
        );
        assert!(!writer.resources.contains_key(&id));
    }

    #[test]
    fn ordinary_incremental_writer_keeps_its_existing_dat_allocation() {
        let fixture = Fixture::new();
        let bytes = b"ordinary incremental content";
        let id = ContentHash::from_data(bytes);
        let mut writer = KFCWriter::new_incremental(
            &fixture.source,
            "game",
            &fixture.reference,
            &fixture.registry,
        )
        .unwrap();
        writer.write_content(&id, bytes).unwrap();
        writer.finalize().unwrap();

        let updated = KFCFile::from_path(fixture.source.join("game.kfc"), false).unwrap();
        assert_eq!(updated.contents().get(&id).unwrap().container_index, 1);
        assert_eq!(
            updated
                .contents()
                .get(&fixture.old_content)
                .unwrap()
                .container_index,
            0
        );
        assert!(fixture.source.join("game_001.dat").is_file());
    }
}
