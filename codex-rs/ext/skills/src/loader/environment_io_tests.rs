use std::fs;
use std::sync::Arc;
use std::time::Duration;

use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::FileSystemEnvironmentAccessor;
use codex_exec_server::LOCAL_FS;
use codex_skills::EnvironmentSkillMetadata;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

use crate::loader::io_test_support::FileSystemCalls;
use crate::loader::io_test_support::ManifestMetadataBehavior;
use crate::loader::io_test_support::RecordingFileSystem;

use super::load_environment_skills_from_root;

