use std::path::Path;

/// A language that at least one project formatter knows how to format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    TypeScript,
    TypeScriptReact,
    JavaScript,
    JavaScriptReact,
    Json,
    Jsonc,
    Css,
    Scss,
    Less,
    Html,
    Vue,
    Svelte,
    Markdown,
    Mdx,
    Yaml,
    GraphQl,
    Python,
    Terraform,
    Shell,
    Toml,
    Go,
    Rust,
    Php,
    Blade,
    Sql,
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Self> {
        let file_name = path.file_name()?.to_str()?.to_ascii_lowercase();
        if file_name.ends_with(".blade.php") {
            return Some(Self::Blade);
        }
        let (_, extension) = file_name.rsplit_once('.')?;
        Self::from_extension(extension)
    }

    /// Maps a lower-case file extension, without its leading dot, to a language.
    pub fn from_extension(extension: &str) -> Option<Self> {
        let language = match extension {
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::TypeScriptReact,
            "js" | "mjs" | "cjs" => Self::JavaScript,
            "jsx" => Self::JavaScriptReact,
            "json" => Self::Json,
            "jsonc" => Self::Jsonc,
            "css" => Self::Css,
            "scss" => Self::Scss,
            "less" => Self::Less,
            "html" | "htm" => Self::Html,
            "vue" => Self::Vue,
            "svelte" => Self::Svelte,
            "md" | "markdown" => Self::Markdown,
            "mdx" => Self::Mdx,
            "yaml" | "yml" => Self::Yaml,
            "graphql" | "gql" => Self::GraphQl,
            "py" | "pyi" => Self::Python,
            "tf" | "tfvars" => Self::Terraform,
            "sh" | "bash" | "ksh" | "bats" => Self::Shell,
            "toml" => Self::Toml,
            "go" => Self::Go,
            "rs" => Self::Rust,
            "php" => Self::Php,
            "sql" => Self::Sql,
            _ => return None,
        };
        Some(language)
    }

    /// The identifier used for this language in per-language settings. These match the language
    /// identifiers VS Code uses, so overrides read the same in both editors.
    pub fn id(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::TypeScriptReact => "typescriptreact",
            Self::JavaScript => "javascript",
            Self::JavaScriptReact => "javascriptreact",
            Self::Json => "json",
            Self::Jsonc => "jsonc",
            Self::Css => "css",
            Self::Scss => "scss",
            Self::Less => "less",
            Self::Html => "html",
            Self::Vue => "vue",
            Self::Svelte => "svelte",
            Self::Markdown => "markdown",
            Self::Mdx => "mdx",
            Self::Yaml => "yaml",
            Self::GraphQl => "graphql",
            Self::Python => "python",
            Self::Terraform => "terraform",
            Self::Shell => "shellscript",
            Self::Toml => "toml",
            Self::Go => "go",
            Self::Rust => "rust",
            Self::Php => "php",
            Self::Blade => "blade",
            Self::Sql => "sql",
        }
    }

    /// The formatters that can handle this language, most preferred first.
    pub fn candidates(self) -> &'static [Tool] {
        match self {
            Self::TypeScript
            | Self::TypeScriptReact
            | Self::JavaScript
            | Self::JavaScriptReact
            | Self::Json
            | Self::Jsonc => &[Tool::Prettier, Tool::Biome, Tool::Dprint],
            Self::Css | Self::GraphQl => &[Tool::Prettier, Tool::Biome],
            Self::Markdown => &[Tool::Prettier, Tool::Dprint],
            Self::Scss
            | Self::Less
            | Self::Html
            | Self::Vue
            | Self::Svelte
            | Self::Mdx
            | Self::Yaml => &[Tool::Prettier],
            Self::Python => &[Tool::Ruff, Tool::Black],
            Self::Terraform => &[Tool::Terraform],
            Self::Shell => &[Tool::Shfmt],
            Self::Toml => &[Tool::Taplo],
            Self::Go => &[Tool::Gofmt],
            Self::Rust => &[Tool::Rustfmt],
            Self::Php => &[Tool::Pint, Tool::PhpCsFixer],
            Self::Blade => &[Tool::Prettier, Tool::BladeFormatter],
            Self::Sql => &[Tool::Sqlfluff, Tool::PgFormat],
        }
    }

    /// The Prettier plugin packages, any one of which lets Prettier parse this language. Empty
    /// when Prettier handles the language on its own.
    pub fn required_prettier_plugins(self) -> &'static [&'static str] {
        match self {
            Self::Svelte => &["prettier-plugin-svelte"],
            Self::Blade => &["prettier-plugin-blade", "@shufo/prettier-plugin-blade"],
            Self::TypeScript
            | Self::TypeScriptReact
            | Self::JavaScript
            | Self::JavaScriptReact
            | Self::Json
            | Self::Jsonc
            | Self::Css
            | Self::Scss
            | Self::Less
            | Self::Html
            | Self::Vue
            | Self::Markdown
            | Self::Mdx
            | Self::Yaml
            | Self::GraphQl
            | Self::Python
            | Self::Terraform
            | Self::Shell
            | Self::Toml
            | Self::Go
            | Self::Rust
            | Self::Php
            | Self::Sql => &[],
        }
    }
}

/// An external formatter this crate knows how to locate and run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Prettier,
    Biome,
    Dprint,
    Ruff,
    Black,
    Terraform,
    Shfmt,
    Taplo,
    Gofmt,
    Rustfmt,
    Pint,
    PhpCsFixer,
    BladeFormatter,
    Sqlfluff,
    PgFormat,
}

impl Tool {
    pub const ALL: [Tool; 15] = [
        Tool::Prettier,
        Tool::Biome,
        Tool::Dprint,
        Tool::Ruff,
        Tool::Black,
        Tool::Terraform,
        Tool::Shfmt,
        Tool::Taplo,
        Tool::Gofmt,
        Tool::Rustfmt,
        Tool::Pint,
        Tool::PhpCsFixer,
        Tool::BladeFormatter,
        Tool::Sqlfluff,
        Tool::PgFormat,
    ];

    /// The executable's file name, which is also the name used to select the tool in settings.
    pub fn name(self) -> &'static str {
        match self {
            Self::Prettier => "prettier",
            Self::Biome => "biome",
            Self::Dprint => "dprint",
            Self::Ruff => "ruff",
            Self::Black => "black",
            Self::Terraform => "terraform",
            Self::Shfmt => "shfmt",
            Self::Taplo => "taplo",
            Self::Gofmt => "gofmt",
            Self::Rustfmt => "rustfmt",
            Self::Pint => "pint",
            Self::PhpCsFixer => "php-cs-fixer",
            Self::BladeFormatter => "blade-formatter",
            Self::Sqlfluff => "sqlfluff",
            Self::PgFormat => "pg_format",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
