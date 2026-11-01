//! Choosing a formatter for a file and working out how to invoke it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::config;
use crate::discovery::{SearchScope, executable_path, find_on_path};
use crate::preference::Preference;
use crate::registry::{Language, Tool};

/// The edition handed to rustfmt when the file's crate does not declare one.
const DEFAULT_RUST_EDITION: &str = "2021";

/// One argument of a formatter's command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Literal(OsString),
    /// Replaced by the path of a temporary copy of the buffer, for tools that can only rewrite a
    /// file in place.
    TempCopy,
}

impl<T: Into<OsString>> From<T> for Arg {
    fn from(value: T) -> Self {
        Self::Literal(value.into())
    }
}

/// A fully resolved formatter command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub tool: Tool,
    pub program: PathBuf,
    pub args: Vec<Arg>,
    pub cwd: PathBuf,
    /// The file whose contents are being formatted.
    pub file: PathBuf,
}

impl Invocation {
    /// Whether the tool rewrites a file in place instead of reading stdin and writing stdout.
    pub fn uses_temp_copy(&self) -> bool {
        self.args.contains(&Arg::TempCopy)
    }

    /// The command-line arguments, with `temp_copy` standing in for [`Arg::TempCopy`].
    pub fn rendered_args(&self, temp_copy: &Path) -> Vec<OsString> {
        self.args
            .iter()
            .map(|arg| match arg {
                Arg::Literal(value) => value.clone(),
                Arg::TempCopy => temp_copy.as_os_str().to_owned(),
            })
            .collect()
    }
}

/// What should format a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// No project formatter applies, so formatting is left to the language server.
    LanguageServer,
    /// The user turned formatting off for this file.
    Off,
    External(Invocation),
    /// The user asked for a specific formatter that cannot be used.
    Unavailable {
        tool: String,
        reason: String,
    },
}

/// Picks the formatter for `file`. `path_env` is the user's shell `PATH`, used to find globally
/// installed tools.
pub fn select(
    file: &Path,
    project_root: Option<&Path>,
    path_env: Option<&str>,
    preference: &Preference,
) -> Selection {
    let forced_tool = match preference {
        Preference::Auto => None,
        Preference::Lsp => return Selection::LanguageServer,
        Preference::Off => return Selection::Off,
        Preference::Tool(name) => Some(name),
    };
    let unavailable = |reason: &str| match forced_tool {
        Some(name) => Selection::Unavailable {
            tool: name.clone(),
            reason: reason.to_owned(),
        },
        None => Selection::LanguageServer,
    };

    let Some(language) = Language::from_path(file) else {
        return unavailable("does not format this type of file");
    };
    let planner = Planner {
        file,
        language,
        scope: SearchScope::new(file, project_root),
        path_env,
    };

    let Some(name) = forced_tool else {
        return match planner.auto() {
            Some(invocation) => Selection::External(invocation),
            None => Selection::LanguageServer,
        };
    };
    let Some(tool) = Tool::from_name(name) else {
        return unavailable("is not a known formatter");
    };
    // Prettier is extensible through plugins, so it may be forced for any language.
    if tool != Tool::Prettier && !language.candidates().contains(&tool) {
        return unavailable("does not format this type of file");
    }
    match planner.plan(tool, true) {
        Ok(planned) => Selection::External(planned.invocation),
        Err(Skip::NoExecutable) => unavailable("was not found in the project or on PATH"),
        Err(Skip::NotOptedIn) => unavailable("is not configured for this project"),
    }
}

/// How strongly a project asks for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptIn {
    /// The project carries the tool's own config.
    Configured,
    /// The tool is merely installed, or listed as a dependency.
    Available,
}

struct Planned {
    invocation: Invocation,
    opt_in: OptIn,
}

enum Skip {
    NoExecutable,
    NotOptedIn,
}

struct Planner<'a> {
    file: &'a Path,
    language: Language,
    scope: SearchScope,
    path_env: Option<&'a str>,
}

impl Planner<'_> {
    /// The tool a project opts in to: the first candidate the project configures, and failing that
    /// the first one that is merely available. A configured tool therefore beats a more preferred
    /// one that is only installed, which is how Biome wins over an unconfigured Prettier.
    fn auto(&self) -> Option<Invocation> {
        let mut fallback = None;
        for tool in self.language.candidates() {
            let Ok(planned) = self.plan(*tool, false) else {
                continue;
            };
            match planned.opt_in {
                OptIn::Configured => return Some(planned.invocation),
                OptIn::Available if fallback.is_none() => fallback = Some(planned.invocation),
                OptIn::Available => {}
            }
        }
        fallback
    }

    /// Works out how to run `tool`. When `forced`, the project does not have to opt in to it.
    fn plan(&self, tool: Tool, forced: bool) -> Result<Planned, Skip> {
        match tool {
            Tool::Prettier => self.prettier(forced),
            Tool::Biome => self.biome(forced),
            Tool::Dprint => self.dprint(forced),
            Tool::Ruff => self.ruff(forced),
            Tool::Black => self.black(forced),
            Tool::Terraform => self.terraform(),
            Tool::Shfmt => self.shfmt(),
            Tool::Taplo => self.taplo(),
            Tool::Gofmt => self.gofmt(),
            Tool::Rustfmt => self.rustfmt(),
            Tool::Pint => self.pint(),
            Tool::PhpCsFixer => self.php_cs_fixer(),
            Tool::BladeFormatter => self.blade_formatter(forced),
            Tool::Sqlfluff => self.sqlfluff(forced),
            Tool::PgFormat => self.pg_format(forced),
        }
    }

    fn prettier(&self, forced: bool) -> Result<Planned, Skip> {
        let plugins = self.language.required_prettier_plugins();
        if !forced && !plugins.is_empty() && !config::node_package_available(&self.scope, plugins) {
            return Err(Skip::NotOptedIn);
        }
        let config = config::prettier_config(&self.scope);
        let opt_in = if config.is_some() {
            OptIn::Configured
        } else if forced || config::node_dependency(&self.scope, &["prettier"]) {
            OptIn::Available
        } else {
            return Err(Skip::NotOptedIn);
        };
        let program = self.node_bin("prettier")?;
        // Prettier resolves `.prettierignore` against its working directory.
        let cwd = self
            .scope
            .find_file(&[".prettierignore"])
            .and_then(|ignore| parent_dir(&ignore))
            .unwrap_or_else(|| self.root());
        let mut args = vec!["--stdin-filepath".into(), self.file.into()];
        // Prettier's own ancestor search does not stop at our authorized repository boundary.
        // Pin the selected config (including package.json), or explicitly disable that search.
        if let Some(config) = config {
            args.extend(["--config".into(), config.into()]);
        } else {
            args.push("--no-config".into());
        }
        // EditorConfig also walks ancestors, even with --config. Prettier has no option to
        // constrain that walk to this scope, so use the explicit Prettier config alone.
        args.push("--no-editorconfig".into());
        Ok(self.planned(Tool::Prettier, program, args, cwd, opt_in))
    }

    fn biome(&self, forced: bool) -> Result<Planned, Skip> {
        let config = config::biome_config(&self.scope);
        if config.is_none() && !forced {
            return Err(Skip::NotOptedIn);
        }
        let program = self.node_bin("biome")?;
        let mut stdin_file_path = OsString::from("--stdin-file-path=");
        stdin_file_path.push(self.file);
        Ok(self.planned(
            Tool::Biome,
            program,
            vec!["format".into(), stdin_file_path.into()],
            self.config_dir(config),
            OptIn::Configured,
        ))
    }

    fn dprint(&self, forced: bool) -> Result<Planned, Skip> {
        let config = config::dprint_config(&self.scope);
        if config.is_none() && !forced {
            return Err(Skip::NotOptedIn);
        }
        let program = self.node_bin("dprint")?;
        Ok(self.planned(
            Tool::Dprint,
            program,
            vec!["fmt".into(), "--stdin".into(), self.file.into()],
            self.config_dir(config),
            OptIn::Configured,
        ))
    }

    fn ruff(&self, forced: bool) -> Result<Planned, Skip> {
        let config = config::ruff_config(&self.scope);
        // A project configured for Black and not for Ruff has chosen Black.
        if !forced && config.is_none() && config::black_config(&self.scope).is_some() {
            return Err(Skip::NotOptedIn);
        }
        let program = self.python_bin("ruff", forced || config.is_some())?;
        let opt_in = opt_in_for(&config);
        Ok(self.planned(
            Tool::Ruff,
            program,
            vec![
                "format".into(),
                "--stdin-filename".into(),
                self.file.into(),
                "-".into(),
            ],
            self.config_dir(config),
            opt_in,
        ))
    }

    fn black(&self, forced: bool) -> Result<Planned, Skip> {
        let config = config::black_config(&self.scope);
        let program = self.python_bin("black", forced || config.is_some())?;
        let opt_in = opt_in_for(&config);
        Ok(self.planned(
            Tool::Black,
            program,
            vec![
                "--quiet".into(),
                "--stdin-filename".into(),
                self.file.into(),
                "-".into(),
            ],
            self.config_dir(config),
            opt_in,
        ))
    }

    fn terraform(&self) -> Result<Planned, Skip> {
        let program = self.global_bin("terraform")?;
        Ok(self.planned(
            Tool::Terraform,
            program,
            vec!["fmt".into(), "-no-color".into(), "-".into()],
            self.file_dir(),
            OptIn::Available,
        ))
    }

    fn shfmt(&self) -> Result<Planned, Skip> {
        let program = self.global_bin("shfmt")?;
        // The file name lets shfmt pick the shell dialect and find the file's `.editorconfig`.
        Ok(self.planned(
            Tool::Shfmt,
            program,
            vec!["--filename".into(), self.file.into()],
            self.root(),
            OptIn::Available,
        ))
    }

    fn taplo(&self) -> Result<Planned, Skip> {
        let config = config::taplo_config(&self.scope);
        let program = self.global_bin("taplo")?;
        let opt_in = opt_in_for(&config);
        Ok(self.planned(
            Tool::Taplo,
            program,
            vec![
                "format".into(),
                "--stdin-filepath".into(),
                self.file.into(),
                "-".into(),
            ],
            self.config_dir(config),
            opt_in,
        ))
    }

    fn gofmt(&self) -> Result<Planned, Skip> {
        let program = self.global_bin("gofmt")?;
        Ok(self.planned(
            Tool::Gofmt,
            program,
            Vec::new(),
            self.file_dir(),
            OptIn::Available,
        ))
    }

    fn rustfmt(&self) -> Result<Planned, Skip> {
        let program = self.global_bin("rustfmt")?;
        let edition =
            config::rust_edition(&self.scope).unwrap_or_else(|| DEFAULT_RUST_EDITION.to_owned());
        // For stdin input rustfmt looks for `rustfmt.toml` upwards from its working directory.
        Ok(self.planned(
            Tool::Rustfmt,
            program,
            vec![
                "--edition".into(),
                edition.into(),
                "--emit".into(),
                "stdout".into(),
            ],
            self.file_dir(),
            OptIn::Available,
        ))
    }

    fn pint(&self) -> Result<Planned, Skip> {
        let (program, project_dir) = self.composer_bin("pint")?;
        let config = Some(project_dir.join("pint.json")).filter(|path| path.is_file());
        let opt_in = opt_in_for(&config);
        let mut args: Vec<Arg> = vec!["--quiet".into()];
        if let Some(config) = config {
            args.extend(["--config".into(), config.into()]);
        }
        args.push(Arg::TempCopy);
        Ok(self.planned(Tool::Pint, program, args, project_dir, opt_in))
    }

    fn php_cs_fixer(&self) -> Result<Planned, Skip> {
        let (program, project_dir) = self.composer_bin("php-cs-fixer")?;
        let config = [".php-cs-fixer.php", ".php-cs-fixer.dist.php"]
            .iter()
            .map(|name| project_dir.join(name))
            .find(|path| path.is_file());
        let opt_in = opt_in_for(&config);
        let mut args: Vec<Arg> = vec![
            "fix".into(),
            "--using-cache=no".into(),
            "--quiet".into(),
            "--no-interaction".into(),
        ];
        if let Some(config) = config {
            let mut config_arg = OsString::from("--config=");
            config_arg.push(config);
            args.push(config_arg.into());
        }
        args.push(Arg::TempCopy);
        Ok(self.planned(Tool::PhpCsFixer, program, args, project_dir, opt_in))
    }

    fn blade_formatter(&self, forced: bool) -> Result<Planned, Skip> {
        let local = self
            .scope
            .find_executable(&["node_modules/.bin/blade-formatter"]);
        let global = || {
            forced
                .then(|| find_on_path("blade-formatter", self.path_env))
                .flatten()
        };
        let program = local.or_else(global).ok_or(Skip::NoExecutable)?;
        let config = config::blade_formatter_config(&self.scope);
        let opt_in = opt_in_for(&config);
        Ok(self.planned(
            Tool::BladeFormatter,
            program,
            vec!["--stdin".into()],
            self.config_dir(config),
            opt_in,
        ))
    }

    fn sqlfluff(&self, forced: bool) -> Result<Planned, Skip> {
        let config = config::sqlfluff_config(&self.scope);
        if config.is_none() && !forced {
            return Err(Skip::NotOptedIn);
        }
        let program = self.python_bin("sqlfluff", true)?;
        Ok(self.planned(
            Tool::Sqlfluff,
            program,
            vec![
                "format".into(),
                "--nocolor".into(),
                "--stdin-filename".into(),
                self.file.into(),
                "-".into(),
            ],
            self.config_dir(config),
            OptIn::Configured,
        ))
    }

    fn pg_format(&self, forced: bool) -> Result<Planned, Skip> {
        let config = config::pg_format_config(&self.scope);
        if config.is_none() && !forced {
            return Err(Skip::NotOptedIn);
        }
        let program = self.global_bin("pg_format")?;
        let mut args: Vec<Arg> = Vec::new();
        if let Some(config) = &config {
            args.extend(["--config".into(), config.into()]);
        }
        args.push("-".into());
        Ok(self.planned(
            Tool::PgFormat,
            program,
            args,
            self.config_dir(config),
            OptIn::Configured,
        ))
    }

    fn planned(
        &self,
        tool: Tool,
        program: PathBuf,
        args: Vec<Arg>,
        cwd: PathBuf,
        opt_in: OptIn,
    ) -> Planned {
        Planned {
            invocation: Invocation {
                tool,
                program,
                args,
                cwd,
                file: self.file.to_path_buf(),
            },
            opt_in,
        }
    }

    fn file_dir(&self) -> PathBuf {
        parent_dir(self.file).unwrap_or_default()
    }

    fn root(&self) -> PathBuf {
        self.scope
            .root()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.file_dir())
    }

    /// The directory a tool should run in so that it picks up `config`.
    fn config_dir(&self, config: Option<PathBuf>) -> PathBuf {
        config
            .and_then(|config| parent_dir(&config))
            .unwrap_or_else(|| self.root())
    }

    fn global_bin(&self, name: &str) -> Result<PathBuf, Skip> {
        find_on_path(name, self.path_env).ok_or(Skip::NoExecutable)
    }

    fn node_bin(&self, name: &str) -> Result<PathBuf, Skip> {
        self.scope
            .find_executable(&[&format!("node_modules/.bin/{name}")])
            .map_or_else(|| self.global_bin(name), Ok)
    }

    /// A Python tool from the project's virtual environment, or failing that from a global install
    /// when `allow_global`. A tool that merely happens to be installed on the machine says nothing
    /// about how a project wants its files laid out, so a project has to ask for it.
    fn python_bin(&self, name: &str, allow_global: bool) -> Result<PathBuf, Skip> {
        let bin_dir = if cfg!(windows) { "Scripts" } else { "bin" };
        let local = self.scope.find_executable(&[
            &format!(".venv/{bin_dir}/{name}"),
            &format!("venv/{bin_dir}/{name}"),
        ]);
        match local {
            Some(program) => Ok(program),
            None if allow_global => self.global_bin(name),
            None => Err(Skip::NotOptedIn),
        }
    }

    /// A Composer-installed executable together with the project directory that holds `vendor`.
    /// These tools are only ever used from the project, never from a global install.
    fn composer_bin(&self, name: &str) -> Result<(PathBuf, PathBuf), Skip> {
        self.scope
            .dirs()
            .find_map(|dir| {
                let program = dir.join("vendor").join("bin").join(name);
                executable_path(&program).map(|program| (program, dir.to_path_buf()))
            })
            .ok_or(Skip::NoExecutable)
    }
}

fn opt_in_for(config: &Option<PathBuf>) -> OptIn {
    match config {
        Some(_) => OptIn::Configured,
        None => OptIn::Available,
    }
}

fn parent_dir(path: &Path) -> Option<PathBuf> {
    path.parent().map(Path::to_path_buf)
}

// The tests drive stand-in formatters written as shell scripts.
#[cfg(all(test, unix))]
#[path = "plan_tests.rs"]
mod tests;
