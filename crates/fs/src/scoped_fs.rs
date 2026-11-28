use super::*;
use std::io::ErrorKind;

#[derive(Clone)]
struct Roots {
    project: PathBuf,
    dependencies: Vec<PathBuf>,
}

impl Roots {
    fn new(project: &Path, dependencies: &[PathBuf]) -> Result<Self> {
        let project =
            std::fs::canonicalize(project).context("Resolving the workspace directory")?;
        anyhow::ensure!(project.is_dir(), "Workspace root must be a directory");
        let dependencies = dependencies
            .iter()
            .filter_map(|path| std::fs::canonicalize(path).ok())
            .filter(|path| path.is_dir() && path.parent().is_some())
            .collect();
        Ok(Self {
            project,
            dependencies,
        })
    }

    fn resolve(&self, path: &Path, write: bool) -> Result<PathBuf> {
        anyhow::ensure!(path.is_absolute(), "Workspace paths must be absolute");
        let mut existing = path;
        let mut suffix = Vec::new();
        let real = loop {
            match std::fs::canonicalize(existing) {
                Ok(path) => break path,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    if std::fs::symlink_metadata(existing).is_ok() {
                        bail!("Cannot resolve the workspace path");
                    }
                    let name = existing.file_name().context("Missing path component")?;
                    #[cfg(windows)]
                    anyhow::ensure!(
                        !name.to_string_lossy().contains([':', '\\']),
                        "Invalid Windows workspace path"
                    );
                    suffix.push(name);
                    existing = existing.parent().context("Missing workspace path parent")?;
                }
                Err(error) => return Err(error.into()),
            }
        };
        anyhow::ensure!(
            real.starts_with(&self.project)
                || (!write && self.dependencies.iter().any(|root| real.starts_with(root))),
            "Path is outside the authorized workspace"
        );
        let mut resolved = real;
        for name in suffix.into_iter().rev() {
            resolved.push(name);
        }
        Ok(resolved)
    }
}

/// Restricts the filesystem shown to a browser-attached headless project.
pub struct ScopedFs {
    inner: Arc<dyn Fs>,
    roots: Roots,
}

impl ScopedFs {
    pub fn new(inner: Arc<dyn Fs>, project: &Path, dependencies: &[PathBuf]) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            inner,
            roots: Roots::new(project, dependencies)?,
        }))
    }

    fn read_path(&self, path: &Path) -> Result<PathBuf> {
        self.roots.resolve(path, false)
    }

    fn write_path(&self, path: &Path) -> Result<PathBuf> {
        self.roots.resolve(path, true)
    }
}

struct ScopedWatcher {
    inner: Option<Arc<dyn Watcher>>,
    roots: Roots,
}

impl Watcher for ScopedWatcher {
    fn add(&self, path: &Path) -> Result<()> {
        let path = self.roots.resolve(path, false)?;
        self.inner
            .as_ref()
            .context("Workspace watcher is unavailable")?
            .add(&path)
    }

    fn remove(&self, path: &Path) -> Result<()> {
        let path = self.roots.resolve(path, false)?;
        self.inner
            .as_ref()
            .context("Workspace watcher is unavailable")?
            .remove(&path)
    }
}

#[async_trait::async_trait]
impl Fs for ScopedFs {
    fn allows_write(&self, path: &Path) -> bool {
        self.write_path(path).is_ok()
    }

    async fn create_dir(&self, path: &Path) -> Result<()> {
        self.inner.create_dir(&self.write_path(path)?).await
    }

    async fn create_symlink(&self, _: &Path, _: PathBuf) -> Result<()> {
        bail!("Creating links is unavailable in a browser workspace")
    }

    async fn create_file(&self, path: &Path, options: CreateOptions) -> Result<()> {
        self.inner
            .create_file(&self.write_path(path)?, options)
            .await
    }

    async fn create_file_with(
        &self,
        path: &Path,
        content: Pin<&mut (dyn AsyncRead + Send)>,
    ) -> Result<()> {
        self.inner
            .create_file_with(&self.write_path(path)?, content)
            .await
    }

    async fn extract_tar_file(
        &self,
        _: &Path,
        _: Archive<Pin<&mut (dyn AsyncRead + Send)>>,
    ) -> Result<()> {
        bail!("Extracting archives is unavailable in a browser workspace")
    }

    async fn copy_file(&self, source: &Path, target: &Path, options: CopyOptions) -> Result<()> {
        self.inner
            .copy_file(&self.read_path(source)?, &self.write_path(target)?, options)
            .await
    }

    async fn rename(&self, source: &Path, target: &Path, options: RenameOptions) -> Result<()> {
        self.inner
            .rename(
                &self.write_path(source)?,
                &self.write_path(target)?,
                options,
            )
            .await
    }

    async fn remove_dir(&self, path: &Path, options: RemoveOptions) -> Result<()> {
        let path = self.write_path(path)?;
        anyhow::ensure!(
            path != self.roots.project,
            "Cannot remove the workspace root"
        );
        self.inner.remove_dir(&path, options).await
    }

    async fn trash(&self, _: &Path, _: RemoveOptions) -> Result<TrashId> {
        bail!("System trash is unavailable in a browser workspace")
    }

    async fn remove_file(&self, path: &Path, options: RemoveOptions) -> Result<()> {
        self.inner
            .remove_file(&self.write_path(path)?, options)
            .await
    }

    async fn open_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>> {
        self.inner.open_handle(&self.read_path(path)?).await
    }

    async fn open_sync(&self, path: &Path) -> Result<Box<dyn ReadSeek + Send + Sync>> {
        self.inner.open_sync(&self.read_path(path)?).await
    }

    async fn load_bytes(&self, path: &Path) -> Result<Vec<u8>> {
        self.inner.load_bytes(&self.read_path(path)?).await
    }

    async fn atomic_write(&self, path: PathBuf, text: String) -> Result<()> {
        self.inner.atomic_write(self.write_path(&path)?, text).await
    }

    async fn save(&self, path: &Path, text: &Rope, line_ending: LineEnding) -> Result<()> {
        let path = self.write_path(path)?;
        if let Some(parent) = path.parent() {
            self.inner.create_dir(parent).await?;
        }
        self.inner
            .atomic_write(
                path,
                text::chunks_with_line_ending(text, line_ending).collect(),
            )
            .await
    }

    async fn write(&self, path: &Path, content: &[u8]) -> Result<()> {
        self.inner.write(&self.write_path(path)?, content).await
    }

    async fn canonicalize(&self, path: &Path) -> Result<PathBuf> {
        self.inner.canonicalize(&self.read_path(path)?).await
    }

    async fn is_file(&self, path: &Path) -> bool {
        match self.read_path(path) {
            Ok(path) => self.inner.is_file(&path).await,
            Err(_) => false,
        }
    }

    async fn is_dir(&self, path: &Path) -> bool {
        match self.read_path(path) {
            Ok(path) => self.inner.is_dir(&path).await,
            Err(_) => false,
        }
    }

    async fn metadata(&self, path: &Path) -> Result<Option<Metadata>> {
        let Ok(path) = self.read_path(path) else {
            return Ok(None);
        };
        self.inner.metadata(&path).await
    }

    async fn read_link(&self, path: &Path) -> Result<PathBuf> {
        self.read_path(path)?;
        self.inner.read_link(path).await
    }

    async fn read_dir(
        &self,
        path: &Path,
    ) -> Result<Pin<Box<dyn Send + Stream<Item = Result<PathBuf>>>>> {
        let entries = self.inner.read_dir(&self.read_path(path)?).await?;
        let roots = self.roots.clone();
        // Callers match children relative to the directory they requested.
        let directory = path.to_path_buf();
        Ok(Box::pin(entries.filter_map(move |entry| {
            let entry = match entry {
                Ok(path) if roots.resolve(&path, false).is_ok() => Some(
                    path.file_name()
                        .context("Directory entry has no file name")
                        .map(|name| directory.join(name)),
                ),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            };
            futures::future::ready(entry)
        })))
    }

    fn start_native_watcher(&self) -> Result<()> {
        self.inner.start_native_watcher()
    }

    fn path_exists(&self, path: &Path) -> bool {
        self.read_path(path)
            .is_ok_and(|path| self.inner.path_exists(&path))
    }

    fn is_path_case_sensitive(&self, path: &Path) -> bool {
        self.read_path(path)
            .is_ok_and(|path| self.inner.is_path_case_sensitive(&path))
    }

    fn requires_poll_watcher(&self, path: &Path) -> bool {
        self.read_path(path)
            .is_ok_and(|path| self.inner.requires_poll_watcher(&path))
    }

    async fn watch(
        &self,
        path: &Path,
        latency: Duration,
    ) -> (
        Pin<Box<dyn Send + Stream<Item = Vec<PathEvent>>>>,
        Arc<dyn Watcher>,
    ) {
        let (events, inner) = match self.read_path(path) {
            Ok(path) => {
                let (events, watcher) = self.inner.watch(&path, latency).await;
                (events, Some(watcher))
            }
            Err(_) => (
                Box::pin(futures::stream::pending())
                    as Pin<Box<dyn Send + Stream<Item = Vec<PathEvent>>>>,
                None,
            ),
        };
        (
            events,
            Arc::new(ScopedWatcher {
                inner,
                roots: self.roots.clone(),
            }),
        )
    }

    fn is_fake(&self) -> bool {
        self.inner.is_fake()
    }

    async fn is_case_sensitive(&self) -> bool {
        self.inner.is_case_sensitive().await
    }

    fn subscribe_to_jobs(&self) -> JobEventReceiver {
        self.inner.subscribe_to_jobs()
    }

    fn original_path_for_trash_id(&self, _: TrashId) -> Option<PathBuf> {
        None
    }

    async fn restore(&self, _: TrashId) -> std::result::Result<PathBuf, TrashRestoreError> {
        Err(TrashRestoreError::Unknown {
            description: "System trash is unavailable in a browser workspace".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[gpui::test]
    async fn scoped_read_dir_preserves_the_callers_directory(executor: BackgroundExecutor) {
        use futures::TryStreamExt as _;

        let parent = tempfile::tempdir().expect("create a temporary directory");
        let project = dunce::simplified(parent.path());
        let ignore_file = project.join(".gitignore");
        std::fs::write(&ignore_file, "ignored.txt\n").expect("write the ignore file");
        let fs = ScopedFs::new(RealFs::new(None, executor), project, &[])
            .expect("create the scoped filesystem");

        let entries: Vec<_> = fs
            .read_dir(project)
            .await
            .expect("read the scoped directory")
            .try_collect()
            .await
            .expect("collect the scoped directory entries");
        assert_eq!(entries, [ignore_file]);
    }

    #[test]
    fn scoped_paths_allow_project_writes_and_only_dependency_reads() -> Result<()> {
        let parent = tempfile::tempdir()?;
        let project = parent.path().join("project");
        let dependencies = parent.path().join("sdk");
        std::fs::create_dir(&project)?;
        std::fs::create_dir(&dependencies)?;
        let definition = dependencies.join("definition.go");
        std::fs::write(&definition, "package sdk")?;
        let roots = Roots::new(&project, &[dependencies])?;
        assert!(
            roots
                .resolve(&project.join("new").join("file.go"), true)
                .is_ok()
        );
        assert!(roots.resolve(&definition, false).is_ok());
        assert!(roots.resolve(&definition, true).is_err());
        assert!(
            roots
                .resolve(&parent.path().join("unrelated.txt"), false)
                .is_err()
        );
        Ok(())
    }
}
