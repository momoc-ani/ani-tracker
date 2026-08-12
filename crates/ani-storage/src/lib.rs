mod error;
mod migration;
mod repository;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ani_domain::SecureStore;
use ani_repository::{RepositoryError, RepositoryResult, UnitOfWork, UnitOfWorkFactory};
use chrono::{SecondsFormat, Utc};
use log::{error, info, warn};
use rusqlite::{backup::Backup, Connection, OpenFlags, Transaction};
use serde_json::Value;

pub use error::{SecureStoreError, StorageError};
use migration::{initialize_database, read_database_versions};
pub use repository::SqliteRepository;

/// 当前与 TypeScript 共用的 SQLite 结构版本。
pub const SQLITE_SCHEMA_VERSION: u32 = 23;
/// 当前与 TypeScript 共用的应用数据版本。
pub const APP_DATA_VERSION: u32 = 25;

/// 首次启动写入的最小应用数据。
#[derive(Debug, Clone)]

pub struct StorageSeed {
    pub settings: Value,
    pub dashboard: Value,
    pub release_sources: Vec<ReleaseSourceSeed>,
}

impl Default for StorageSeed {
    /// 创建不含演示业务数据的空种子。
    fn default() -> Self {
        Self {
            settings: Value::Object(Default::default()),
            dashboard: Value::Object(Default::default()),
            release_sources: Vec::new(),
        }
    }
}

/// 首次启动写入的下载源配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSourceSeed {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub use_proxy: bool,
    pub request_interval_ms: i64,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub rss_url: Option<String>,
    pub tags: Vec<String>,
}

/// SQLite 启动参数，由宿主提供平台路径和默认数据。
#[derive(Debug, Clone)]
pub struct StorageOptions {
    pub database_path: PathBuf,
    pub backup_directory: PathBuf,
    pub legacy_database_paths: Vec<PathBuf>,
    pub seed: StorageSeed,
}

/// 本次 SQLite 启动执行的复制、迁移和备份结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageOpenReport {
    pub created: bool,
    pub copied_from: Option<PathBuf>,
    pub backup_path: Option<PathBuf>,
    pub schema_version: u32,
    pub app_data_version: u32,
}

/// 持有单写者 SQLite 连接。
pub struct Storage {
    connection: Connection,
    database_path: PathBuf,
    backup_directory: PathBuf,
    seed: StorageSeed,
    secure_store: Option<Arc<dyn SecureStore<Error = SecureStoreError>>>,
    report: StorageOpenReport,
}

/// SQLite 事务实现的工作单元，未提交即按 rusqlite 语义回滚。
pub struct SqliteUnitOfWork<'connection> {
    transaction: Option<Transaction<'connection>>,
    secure_store: Option<&'connection dyn SecureStore<Error = SecureStoreError>>,
}

impl Storage {
    /// 发现旧 Electron 数据库，完成一致性复制、迁移、校验和失败恢复。
    pub fn open(options: StorageOptions) -> Result<Self, StorageError> {
        ensure_parent_directory(&options.database_path)?;
        ensure_directory(&options.backup_directory)?;

        let copied_from =
            copy_legacy_database_if_needed(&options.database_path, &options.legacy_database_paths)?;
        let existed_before = options.database_path.exists();
        let created = copied_from.is_none() && !existed_before;
        let mut connection = open_connection(&options.database_path)?;
        verify_integrity(&connection, &options.database_path)?;

        let versions = read_database_versions(&connection)?;
        if let Some(actual) = versions
            .schema_version
            .filter(|version| *version > SQLITE_SCHEMA_VERSION)
        {
            return Err(StorageError::UnsupportedSchemaVersion {
                actual,
                supported: SQLITE_SCHEMA_VERSION,
            });
        }
        if let Some(actual) = versions
            .app_data_version
            .filter(|version| *version > APP_DATA_VERSION)
        {
            return Err(StorageError::UnsupportedAppDataVersion {
                actual,
                supported: APP_DATA_VERSION,
            });
        }

        let needs_migration = versions.schema_version != Some(SQLITE_SCHEMA_VERSION)
            || versions.app_data_version != Some(APP_DATA_VERSION);
        let backup_path = if existed_before && (needs_migration || copied_from.is_some()) {
            Some(create_migration_backup(
                &connection,
                &options.database_path,
                &options.backup_directory,
                versions.schema_version,
                versions.app_data_version,
            )?)
        } else {
            None
        };

        let initialization = initialize_database(&mut connection, &options.seed)
            .and_then(|_| configure_connection(&connection))
            .and_then(|_| verify_integrity(&connection, &options.database_path));
        if let Err(migration_error) = initialization {
            drop(connection);
            if let Some(backup_path) = backup_path.as_deref() {
                return match restore_database(&options.database_path, backup_path) {
                    Ok(()) => {
                        error!(
                            "SQLite 迁移失败，已恢复备份：database={}, backup={}, error={}",
                            options.database_path.display(),
                            backup_path.display(),
                            migration_error
                        );
                        Err(StorageError::MigrationRolledBack {
                            source: Box::new(migration_error),
                        })
                    }
                    Err(restore_error) => Err(StorageError::MigrationRestoreFailed {
                        migration: Box::new(migration_error),
                        restore: Box::new(restore_error),
                    }),
                };
            }

            if created {
                remove_database_files(&options.database_path);
            }
            return Err(migration_error);
        }

        let report = StorageOpenReport {
            created,
            copied_from,
            backup_path,
            schema_version: SQLITE_SCHEMA_VERSION,
            app_data_version: APP_DATA_VERSION,
        };
        info!(
            "SQLite 数据层就绪：database={}, created={}, copied_from={:?}, backup={:?}",
            options.database_path.display(),
            report.created,
            report.copied_from,
            report.backup_path
        );

        Ok(Self {
            connection,
            database_path: options.database_path,
            backup_directory: options.backup_directory,
            seed: options.seed,
            secure_store: None,
            report,
        })
    }

    /// 返回本次启动的数据库迁移报告。
    pub fn report(&self) -> &StorageOpenReport {
        &self.report
    }

    /// 返回当前数据库文件路径。
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// 执行完整性与外键一致性检查。
    pub fn verify(&self) -> Result<(), StorageError> {
        verify_integrity(&self.connection, &self.database_path)
    }

    /// 创建仅在当前连接生命周期内有效的业务 Repository。
    pub fn repository(&self) -> SqliteRepository<'_> {
        SqliteRepository::new(&self.connection, self.secure_store.as_deref())
    }

    /// 装配平台安全存储；后续 Repository 会透明迁移并解析敏感字段。
    pub fn set_secure_store(
        &mut self,
        secure_store: Arc<dyn SecureStore<Error = SecureStoreError>>,
    ) {
        self.secure_store = Some(secure_store);
    }

    /// 创建包含当前 WAL 内容的手动一致性备份。
    pub fn create_manual_backup(&self) -> Result<PathBuf, StorageError> {
        let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        let file_name = format!("ani-tracker.manual-{timestamp}.sqlite");
        let path = unique_path(&self.backup_directory, &file_name);
        snapshot_database(&self.connection, &path)?;
        info!("SQLite 手动备份完成：backup={}", path.display());
        Ok(path)
    }

    /// 将当前数据库导出到指定路径，目标存在时原子替换其内容。
    pub fn export_to(&self, target: &Path) -> Result<(), StorageError> {
        ensure_not_active_database(target, &self.database_path, "backupPath")?;
        ensure_parent_directory(target)?;
        snapshot_database(&self.connection, target)
    }

    /// 迁移并校验外部备份，再以操作前快照保护活动数据库恢复。
    pub fn restore_from(&mut self, source: &Path) -> Result<PathBuf, StorageError> {
        if !source.is_file() {
            return Err(StorageError::InvalidInput {
                field: "backupPath",
                message: "备份文件不存在".to_owned(),
            });
        }
        ensure_not_active_database(source, &self.database_path, "backupPath")?;
        let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        let staging_path = unique_path(
            &self.backup_directory,
            &format!("ani-tracker.restore-staging-{timestamp}.sqlite"),
        );
        fs::copy(source, &staging_path)
            .map_err(|error| StorageError::file("复制待恢复备份", source, error))?;

        let migrated = Storage::open(StorageOptions {
            database_path: staging_path.clone(),
            backup_directory: self.backup_directory.clone(),
            legacy_database_paths: Vec::new(),
            seed: self.seed.clone(),
        });
        let migrated = match migrated {
            Ok(storage) => storage,
            Err(error) => {
                remove_database_files(&staging_path);
                return Err(error);
            }
        };
        let rollback_path = unique_path(
            &self.backup_directory,
            &format!("ani-tracker.pre-restore-{timestamp}.sqlite"),
        );
        snapshot_database(&self.connection, &rollback_path)?;

        let restore_result = replace_database(&mut self.connection, &migrated.connection)
            .and_then(|_| configure_connection(&self.connection))
            .and_then(|_| verify_integrity(&self.connection, &self.database_path));
        drop(migrated);
        remove_database_files(&staging_path);

        if let Err(restore_error) = restore_result {
            let rollback_result = Connection::open_with_flags(
                &rollback_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(StorageError::from)
            .and_then(|rollback| replace_database(&mut self.connection, &rollback))
            .and_then(|_| configure_connection(&self.connection))
            .and_then(|_| verify_integrity(&self.connection, &self.database_path));
            return match rollback_result {
                Ok(()) => Err(StorageError::DataRestoreRolledBack {
                    source: Box::new(restore_error),
                }),
                Err(rollback_error) => Err(StorageError::DataRestoreFailed {
                    restore: Box::new(restore_error),
                    rollback: Box::new(rollback_error),
                }),
            };
        }

        info!(
            "SQLite 用户备份恢复完成：source={}, rollback={}",
            source.display(),
            rollback_path.display()
        );
        Ok(rollback_path)
    }
}

/// 防止导出或恢复操作覆盖当前正在使用的数据库文件。
fn ensure_not_active_database(
    candidate: &Path,
    database_path: &Path,
    field: &'static str,
) -> Result<(), StorageError> {
    let candidate = fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    let database_path =
        fs::canonicalize(database_path).unwrap_or_else(|_| database_path.to_path_buf());
    if candidate == database_path {
        return Err(StorageError::InvalidInput {
            field,
            message: "不能选择当前正在使用的数据库文件".to_owned(),
        });
    }
    Ok(())
}

impl UnitOfWorkFactory for Storage {
    type Work<'work> = SqliteUnitOfWork<'work>;

    /// 在 SQLite 单写者连接上开始显式事务。
    fn begin_unit_of_work(&mut self) -> RepositoryResult<Self::Work<'_>> {
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::from)
            .map_err(RepositoryError::from)?;
        Ok(SqliteUnitOfWork {
            transaction: Some(transaction),
            secure_store: self.secure_store.as_deref(),
        })
    }
}

impl UnitOfWork for SqliteUnitOfWork<'_> {
    type Repositories<'repository>
        = SqliteRepository<'repository>
    where
        Self: 'repository;

    /// 返回复用当前 SQLite 事务的 Repository 集合。
    fn repositories(&self) -> Self::Repositories<'_> {
        let transaction = self
            .transaction
            .as_ref()
            .expect("unit of work transaction must exist before completion");
        SqliteRepository::in_unit_of_work(transaction, self.secure_store)
    }

    /// 提交 SQLite 工作单元。
    fn commit(mut self) -> RepositoryResult<()> {
        self.transaction
            .take()
            .expect("unit of work transaction must exist before commit")
            .commit()
            .map_err(StorageError::from)
            .map_err(RepositoryError::from)
    }

    /// 回滚 SQLite 工作单元。
    fn rollback(mut self) -> RepositoryResult<()> {
        self.transaction
            .take()
            .expect("unit of work transaction must exist before rollback")
            .rollback()
            .map_err(StorageError::from)
            .map_err(RepositoryError::from)
    }
}

impl Drop for Storage {
    /// 关闭前尽力将 WAL 内容写回主数据库。
    fn drop(&mut self) {
        if let Err(checkpoint_error) = self
            .connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        {
            warn!(
                "SQLite WAL 收尾失败：database={}, error={}",
                self.database_path.display(),
                checkpoint_error
            );
        }
    }
}

/// 打开现有或新建 SQLite 连接。
fn open_connection(path: &Path) -> Result<Connection, StorageError> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(StorageError::from)
}

/// 配置单写者 SQLite 连接参数。
fn configure_connection(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;\
         PRAGMA foreign_keys = ON;\
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

/// 校验数据库页结构与全部外键引用。
fn verify_integrity(connection: &Connection, path: &Path) -> Result<(), StorageError> {
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|source| StorageError::CorruptDatabase {
            path: path.to_path_buf(),
            detail: source.to_string(),
        })?;
    if integrity != "ok" {
        return Err(StorageError::CorruptDatabase {
            path: path.to_path_buf(),
            detail: integrity,
        });
    }

    let foreign_key_error_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if foreign_key_error_count > 0 {
        return Err(StorageError::CorruptDatabase {
            path: path.to_path_buf(),
            detail: format!("存在 {foreign_key_error_count} 条无效外键引用"),
        });
    }
    Ok(())
}

/// 目标库不存在时，从第一个有效旧库创建一致性副本。
fn copy_legacy_database_if_needed(
    target: &Path,
    candidates: &[PathBuf],
) -> Result<Option<PathBuf>, StorageError> {
    if target.exists() {
        return Ok(None);
    }

    let source = candidates
        .iter()
        .find(|candidate| candidate.as_path() != target && candidate.is_file());
    let Some(source) = source else {
        return Ok(None);
    };

    let source_connection = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    verify_integrity(&source_connection, source)?;
    snapshot_database(&source_connection, target)?;
    info!(
        "已复制 Electron SQLite 数据库：source={}, target={}",
        source.display(),
        target.display()
    );
    Ok(Some(source.clone()))
}

/// 使用 SQLite 在线备份 API 创建包含 WAL 内容的一致性快照。
fn snapshot_database(connection: &Connection, target: &Path) -> Result<(), StorageError> {
    if target.exists() {
        fs::remove_file(target)
            .map_err(|source| StorageError::file("删除旧快照", target, source))?;
    }
    let mut destination = Connection::open(target)?;
    let backup = Backup::new(connection, &mut destination)?;
    backup.run_to_completion(128, std::time::Duration::from_millis(10), None)?;
    drop(backup);
    destination
        .close()
        .map_err(|(_, error)| StorageError::Sqlite(error))?;
    Ok(())
}

/// 使用 SQLite 在线备份 API 将来源完整覆盖到现有活动连接。
fn replace_database(destination: &mut Connection, source: &Connection) -> Result<(), StorageError> {
    let backup = Backup::new(source, destination)?;
    backup.run_to_completion(128, std::time::Duration::from_millis(10), None)?;
    drop(backup);
    Ok(())
}

/// 在迁移前生成带源版本号的数据库备份。
fn create_migration_backup(
    connection: &Connection,
    database_path: &Path,
    backup_directory: &Path,
    schema_version: Option<u32>,
    app_data_version: Option<u32>,
) -> Result<PathBuf, StorageError> {
    let stem = database_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("ani-tracker");
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let base_name = format!(
        "{stem}.schema-{}.app-{}.migration-{timestamp}.sqlite",
        schema_version.unwrap_or(0),
        app_data_version.unwrap_or(0)
    );
    let backup_path = unique_path(backup_directory, &base_name);
    snapshot_database(connection, &backup_path)?;
    info!(
        "SQLite 迁移备份完成：database={}, backup={}",
        database_path.display(),
        backup_path.display()
    );
    Ok(backup_path)
}

/// 将迁移前快照恢复为活动数据库。
fn restore_database(database_path: &Path, backup_path: &Path) -> Result<(), StorageError> {
    remove_sidecar_file(database_path, "-wal")?;
    remove_sidecar_file(database_path, "-shm")?;
    fs::copy(backup_path, database_path)
        .map_err(|source| StorageError::file("恢复数据库备份", database_path, source))?;
    Ok(())
}

/// 创建目录并保留具体失败路径。
fn ensure_directory(path: &Path) -> Result<(), StorageError> {
    fs::create_dir_all(path).map_err(|source| StorageError::file("创建目录", path, source))
}

/// 创建数据库文件的父目录。
fn ensure_parent_directory(path: &Path) -> Result<(), StorageError> {
    match path.parent() {
        Some(parent) => ensure_directory(parent),
        None => Ok(()),
    }
}

/// 为同一毫秒内的多个备份选择不冲突路径。
fn unique_path(directory: &Path, file_name: &str) -> PathBuf {
    let initial = directory.join(file_name);
    if !initial.exists() {
        return initial;
    }

    for suffix in 1..=u32::MAX {
        let candidate = directory.join(format!("{file_name}.{suffix}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("u32 backup suffix space exhausted")
}

/// 删除数据库失败初始化产生的精确目标文件。
fn remove_database_files(database_path: &Path) {
    for path in [
        database_path.to_path_buf(),
        sidecar_path(database_path, "-wal"),
        sidecar_path(database_path, "-shm"),
    ] {
        if let Err(remove_error) = fs::remove_file(&path) {
            if remove_error.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    "清理失败的 SQLite 文件失败：path={}, error={}",
                    path.display(),
                    remove_error
                );
            }
        }
    }
}

/// 删除指定 SQLite sidecar 文件。
fn remove_sidecar_file(database_path: &Path, suffix: &str) -> Result<(), StorageError> {
    let path = sidecar_path(database_path, suffix);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StorageError::file("删除 SQLite sidecar", path, source)),
    }
}

/// 生成 SQLite WAL 或 SHM 文件路径。
fn sidecar_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut path = OsString::from(database_path.as_os_str());
    path.push(suffix);
    PathBuf::from(path)
}

/// 返回当前 UTC 时间，供数据库元数据和 seed 共用。
pub(crate) fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests;
