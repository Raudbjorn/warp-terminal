//! Lookup tables mapping file and folder names to bundled icons. All keys are lowercase.
//!
//! The icons come from Material Icon Theme; see `app/assets/bundled/svg/material_icon_theme`.

use super::{FileIcon, FolderIcon};

macro_rules! icon {
    ($name:literal) => {
        FileIcon {
            path: concat!("bundled/svg/material_icon_theme/", $name, ".svg"),
            light_path: None,
        }
    };
    ($name:literal, light) => {
        FileIcon {
            path: concat!("bundled/svg/material_icon_theme/", $name, ".svg"),
            light_path: Some(concat!(
                "bundled/svg/material_icon_theme/",
                $name,
                "_light.svg"
            )),
        }
    };
}

macro_rules! folder {
    ($name:literal) => {
        FolderIcon {
            closed: concat!("bundled/svg/material_icon_theme/folder-", $name, ".svg"),
            open: concat!(
                "bundled/svg/material_icon_theme/folder-",
                $name,
                "-open.svg"
            ),
        }
    };
}

pub(super) const DEFAULT_FILE: &str = "bundled/svg/material_icon_theme/file.svg";

pub(super) const DEFAULT_FOLDER: FolderIcon = FolderIcon {
    closed: "bundled/svg/material_icon_theme/folder.svg",
    open: "bundled/svg/material_icon_theme/folder-open.svg",
};

/// Material Icon Theme has no Cython icon.
const CYTHON: FileIcon = FileIcon {
    path: "bundled/svg/file_type/cython.svg",
    light_path: None,
};

/// Icons for exact file names.
pub(super) const FILE_NAMES: &[(&[&str], FileIcon)] = &[
    (
        &[
            "package.json",
            "package-lock.json",
            ".nvmrc",
            ".node-version",
        ],
        icon!("nodejs"),
    ),
    (&[".npmrc", ".npmignore"], icon!("npm")),
    (&["yarn.lock", ".yarnrc", ".yarnrc.yml"], icon!("yarn")),
    (
        &["pnpm-lock.yaml", "pnpm-workspace.yaml", ".pnpmfile.cjs"],
        icon!("pnpm", light),
    ),
    (
        &["bun.lock", "bun.lockb", "bunfig.toml"],
        icon!("bun", light),
    ),
    (
        &[
            ".gitignore",
            ".gitattributes",
            ".gitmodules",
            ".gitkeep",
            ".gitconfig",
            ".git-blame-ignore-revs",
        ],
        icon!("git"),
    ),
    (&[".gitlab-ci.yml"], icon!("gitlab")),
    (&["gnumakefile"], icon!("makefile")),
    (
        &["go.mod", "go.sum", "go.work", "go.work.sum"],
        icon!("go-mod"),
    ),
    (&["gemfile"], icon!("gemfile")),
    (&["rakefile"], icon!("ruby")),
    (&["artisan"], icon!("laravel")),
    (
        &[
            "requirements.txt",
            "pyproject.toml",
            "pipfile",
            ".python-version",
        ],
        icon!("python-misc"),
    ),
    (&["uv.lock", "uv.toml"], icon!("uv")),
    (&["poetry.lock"], icon!("poetry")),
    (&["claude.md", "claude.local.md"], icon!("claude")),
    (&["agents.md"], icon!("agent")),
    (&["vercel.json", ".vercelignore"], icon!("vercel", light)),
    (&["turbo.json", "turbo.jsonc"], icon!("turborepo", light)),
    (&["biome.json", "biome.jsonc"], icon!("biome")),
    (
        &["supabase.ts", "supabase.js", "supabase.py"],
        icon!("supabase"),
    ),
    (&[".editorconfig"], icon!("editorconfig")),
    (&[".prettierignore"], icon!("prettier")),
    (&[".eslintignore"], icon!("eslint")),
    (&[".envrc"], icon!("tune")),
    (&["favicon.ico"], icon!("favicon")),
    (&["robots.txt"], icon!("robots")),
    (&["todo", "todo.md", "todo.txt", "todos.md"], icon!("todo")),
    (&["codeowners"], icon!("codeowners")),
    (&["citation.cff"], icon!("citation")),
    (&["dependabot.yml", "dependabot.yaml"], icon!("dependabot")),
    (
        &[
            "renovate.json",
            "renovate.json5",
            ".renovaterc",
            ".renovaterc.json",
        ],
        icon!("renovate"),
    ),
    (&["nginx.conf"], icon!("nginx")),
    (&["cmakelists.txt"], icon!("cmake")),
    (
        &["gradlew", "gradlew.bat", "gradle.properties"],
        icon!("gradle"),
    ),
    (&["cname"], icon!("http")),
];

/// A file-name prefix that selects an icon regardless of what follows it.
pub(super) struct NamePrefix {
    pub prefix: &'static str,
    /// Final extensions the rule is limited to, where `""` stands for no extension. Empty means
    /// the rule applies to every extension.
    pub extensions: &'static [&'static str],
    pub icon: FileIcon,
}

const ANY_EXTENSION: &[&str] = &[];
/// Keeps prose-document names such as `license` from claiming source files like
/// `license_checker.py`.
const DOCUMENT_EXTENSIONS: &[&str] = &["", "md", "mdx", "markdown", "txt", "rst"];
const YAML_EXTENSIONS: &[&str] = &["yml", "yaml"];
const JSON_EXTENSIONS: &[&str] = &["json", "jsonc"];

const fn prefix(
    prefix: &'static str,
    extensions: &'static [&'static str],
    icon: FileIcon,
) -> NamePrefix {
    NamePrefix {
        prefix,
        extensions,
        icon,
    }
}

/// Icons for file names that start with a well-known name, such as `Dockerfile.dev`,
/// `.env.local` or `tsconfig.app.json`.
pub(super) const NAME_PREFIXES: &[NamePrefix] = &[
    prefix("readme", DOCUMENT_EXTENSIONS, icon!("readme")),
    prefix("license", DOCUMENT_EXTENSIONS, icon!("license")),
    prefix("licence", DOCUMENT_EXTENSIONS, icon!("license")),
    prefix("copying", DOCUMENT_EXTENSIONS, icon!("license")),
    prefix("changelog", DOCUMENT_EXTENSIONS, icon!("changelog")),
    prefix("contributing", DOCUMENT_EXTENSIONS, icon!("contributing")),
    prefix("code_of_conduct", DOCUMENT_EXTENSIONS, icon!("conduct")),
    prefix("authors", DOCUMENT_EXTENSIONS, icon!("authors")),
    prefix("contributors", DOCUMENT_EXTENSIONS, icon!("authors")),
    prefix("dockerfile", ANY_EXTENSION, icon!("docker")),
    prefix("docker-compose", YAML_EXTENSIONS, icon!("docker")),
    prefix("compose", YAML_EXTENSIONS, icon!("docker")),
    prefix(".env", ANY_EXTENSION, icon!("tune")),
    prefix("makefile", ANY_EXTENSION, icon!("makefile")),
    prefix("tsconfig", JSON_EXTENSIONS, icon!("tsconfig")),
    prefix("jsconfig", JSON_EXTENSIONS, icon!("jsconfig")),
    prefix("vite.config", ANY_EXTENSION, icon!("vite")),
    prefix("vitest.config", ANY_EXTENSION, icon!("vitest")),
    prefix("vitest.workspace", ANY_EXTENSION, icon!("vitest")),
    prefix("next.config", ANY_EXTENSION, icon!("next", light)),
    prefix("tailwind.config", ANY_EXTENSION, icon!("tailwindcss")),
    prefix("postcss.config", ANY_EXTENSION, icon!("postcss")),
    prefix(".postcssrc", ANY_EXTENSION, icon!("postcss")),
    prefix("eslint.config", ANY_EXTENSION, icon!("eslint")),
    prefix(".eslintrc", ANY_EXTENSION, icon!("eslint")),
    prefix("prettier.config", ANY_EXTENSION, icon!("prettier")),
    prefix(".prettierrc", ANY_EXTENSION, icon!("prettier")),
    prefix("jest.config", ANY_EXTENSION, icon!("jest")),
    prefix("playwright.config", ANY_EXTENSION, icon!("playwright")),
    prefix("webpack.config", ANY_EXTENSION, icon!("webpack")),
    prefix("rollup.config", ANY_EXTENSION, icon!("rollup")),
    prefix("babel.config", ANY_EXTENSION, icon!("babel")),
    prefix(".babelrc", ANY_EXTENSION, icon!("babel")),
    prefix("drizzle.config", ANY_EXTENSION, icon!("drizzle")),
    prefix("prisma.config", ANY_EXTENSION, icon!("prisma")),
    prefix("svelte.config", ANY_EXTENSION, icon!("svelte")),
    prefix("astro.config", ANY_EXTENSION, icon!("astro")),
    prefix("nuxt.config", ANY_EXTENSION, icon!("nuxt")),
    prefix("wrangler", &["toml", "json", "jsonc"], icon!("wrangler")),
];

/// Icons for file extensions, without the leading dot. Compound extensions such as `d.ts` take
/// precedence over their final segment.
pub(super) const EXTENSIONS: &[(&[&str], FileIcon)] = &[
    (&["ts", "mts", "cts"], icon!("typescript")),
    (&["d.ts", "d.mts", "d.cts"], icon!("typescript-def")),
    (&["tsx"], icon!("react_ts")),
    (&["js", "mjs", "cjs", "es6"], icon!("javascript")),
    (&["jsx"], icon!("react")),
    (
        &[
            "test.ts",
            "test.mts",
            "test.cts",
            "spec.ts",
            "spec.mts",
            "spec.cts",
            "e2e-spec.ts",
            "cy.ts",
        ],
        icon!("test-ts"),
    ),
    (
        &[
            "test.tsx", "test.jsx", "spec.tsx", "spec.jsx", "cy.tsx", "cy.jsx",
        ],
        icon!("test-jsx"),
    ),
    (
        &[
            "test.js", "test.mjs", "test.cjs", "spec.js", "spec.mjs", "spec.cjs", "cy.js",
        ],
        icon!("test-js"),
    ),
    (&["js.map", "mjs.map", "cjs.map"], icon!("javascript-map")),
    (&["css.map"], icon!("css-map")),
    (
        &[
            "json",
            "jsonc",
            "json5",
            "jsonl",
            "ndjson",
            "geojson",
            "webmanifest",
            "tsbuildinfo",
            "ts.map",
        ],
        icon!("json"),
    ),
    (&["md", "markdown", "rst"], icon!("markdown")),
    (&["mdx"], icon!("mdx")),
    (&["py", "pyi", "pyw"], icon!("python")),
    (&["pyc", "whl", "egg"], icon!("python-misc")),
    (&["pyx", "pxd"], CYTHON),
    (
        &[
            "sql", "sqlite", "sqlite3", "db", "db3", "pgsql", "psql", "mdb", "accdb", "parquet",
        ],
        icon!("database"),
    ),
    (&["php", "phtml"], icon!("php")),
    (&["blade.php"], icon!("laravel")),
    (&["css"], icon!("css")),
    (&["scss", "sass"], icon!("sass")),
    (&["less"], icon!("less")),
    (&["pcss"], icon!("postcss")),
    (&["html", "htm", "xhtml"], icon!("html")),
    (&["yml", "yaml"], icon!("yaml")),
    (&["toml"], icon!("toml", light)),
    (
        &["tf", "tf.json", "tfvars", "tfstate", "tfbackend"],
        icon!("terraform"),
    ),
    (&["hcl"], icon!("hcl", light)),
    (
        &[
            "sh",
            "zsh",
            "bash",
            "fish",
            "ksh",
            "csh",
            "nu",
            "awk",
            "bat",
            "cmd",
            "zshrc",
            "zshenv",
            "zprofile",
            "bashrc",
            "bash_profile",
            "profile",
        ],
        icon!("console"),
    ),
    (&["ps1", "psm1", "psd1"], icon!("powershell")),
    (
        &["dockerfile", "dockerignore", "containerfile"],
        icon!("docker"),
    ),
    (&["env"], icon!("tune")),
    (&["rs", "ron"], icon!("rust")),
    (&["go"], icon!("go")),
    (&["java", "jsp"], icon!("java")),
    (&["rb", "erb", "gemspec", "rake", "ru"], icon!("ruby")),
    (&["vue"], icon!("vue")),
    (&["svelte"], icon!("svelte")),
    (&["astro"], icon!("astro")),
    (&["hbs", "handlebars", "mustache"], icon!("handlebars")),
    (&["twig"], icon!("twig")),
    (&["ejs"], icon!("ejs")),
    (&["jinja", "jinja2", "j2"], icon!("jinja", light)),
    (
        &["csv", "tsv", "xlsx", "xlsm", "xls", "ods"],
        icon!("table"),
    ),
    (&["txt"], icon!("document")),
    (&["pdf"], icon!("pdf")),
    (&["doc", "docx", "rtf", "odt"], icon!("word")),
    (&["ppt", "pptx", "odp"], icon!("powerpoint")),
    (
        &[
            "png", "jpg", "jpeg", "webp", "gif", "ico", "bmp", "tif", "tiff", "avif", "heic",
            "icns",
        ],
        icon!("image"),
    ),
    (&["svg"], icon!("svg")),
    (
        &["ttf", "otf", "woff", "woff2", "eot", "ttc"],
        icon!("font"),
    ),
    (
        &["mp4", "mov", "webm", "mkv", "avi", "m4v", "mpg", "mpeg"],
        icon!("video"),
    ),
    (
        &["mp3", "wav", "flac", "ogg", "m4a", "aac", "opus"],
        icon!("audio"),
    ),
    (
        &[
            "zip", "gz", "tgz", "tar", "bz2", "xz", "7z", "rar", "zst", "br",
        ],
        icon!("zip"),
    ),
    (&["lock"], icon!("lock")),
    (&["mk"], icon!("makefile")),
    (&["tex", "ltx", "latex"], icon!("tex")),
    (&["bib"], icon!("bibliography")),
    (&["typ"], icon!("typst")),
    (&["feature"], icon!("cucumber")),
    (
        &["xml", "plist", "xsd", "xsl", "xslt", "storyboard", "xib"],
        icon!("xml"),
    ),
    (&["ipynb"], icon!("jupyter")),
    (
        &[
            "ini",
            "conf",
            "cfg",
            "cnf",
            "config",
            "properties",
            "tool-versions",
        ],
        icon!("settings"),
    ),
    (&["log"], icon!("log")),
    (&["pem", "key", "pub", "asc", "gpg"], icon!("key")),
    (&["crt", "cer", "cert"], icon!("certificate")),
    (&["kt", "kts"], icon!("kotlin")),
    (&["swift"], icon!("swift")),
    (&["c"], icon!("c")),
    (&["cpp", "cc", "cxx", "c++"], icon!("cpp")),
    (&["h"], icon!("h")),
    (&["hpp", "hh", "hxx"], icon!("hpp")),
    (&["m"], icon!("objective-c")),
    (&["mm"], icon!("objective-cpp")),
    (&["cs", "csx"], icon!("csharp")),
    (&["lua"], icon!("lua")),
    (&["graphql", "gql"], icon!("graphql")),
    (&["prisma"], icon!("prisma")),
    (&["proto"], icon!("proto")),
    (&["exe", "msi"], icon!("exe")),
    (&["wasm", "wat"], icon!("webassembly")),
    (&["zig", "zon"], icon!("zig")),
    (&["pl", "pm"], icon!("perl")),
    (&["ng", "ngml"], icon!("angular")),
    (&["swf"], icon!("flash")),
    (&["dart"], icon!("dart")),
    (&["scala", "sc"], icon!("scala")),
    (&["ex", "exs", "heex"], icon!("elixir")),
    (&["hs"], icon!("haskell")),
    (&["r", "rmd"], icon!("r")),
    (&["nix"], icon!("nix")),
    (&["cmake"], icon!("cmake")),
    (&["gradle"], icon!("gradle")),
    (&["http", "rest"], icon!("http")),
    (&["diff", "patch"], icon!("diff")),
    (&["url"], icon!("url")),
    (&["mmd", "mermaid"], icon!("mermaid")),
    (
        &["glsl", "wgsl", "hlsl", "vert", "frag", "metal"],
        icon!("shader"),
    ),
    (&["vim", "vimrc"], icon!("vim")),
    (&["code-workspace"], icon!("vscode")),
    (&["fig"], icon!("figma")),
    (&["eml"], icon!("email")),
];

/// Icons for folder names. Names are matched after stripping the decoration handled by
/// `undecorated_folder_name`, so `github` also covers `.github` and `tests` covers `__tests__`.
pub(super) const FOLDER_NAMES: &[(&[&str], FolderIcon)] = &[
    (
        &["src", "srcs", "source", "sources", "code"],
        folder!("src"),
    ),
    (
        &[
            "test",
            "tests",
            "testing",
            "spec",
            "specs",
            "e2e",
            "snapshots",
        ],
        folder!("test"),
    ),
    (&["node_modules", "node", "nodejs"], folder!("node")),
    (
        &["git", "githooks", "patches", "submodules"],
        folder!("git"),
    ),
    (&["github"], folder!("github")),
    (&["gitlab"], folder!("gitlab")),
    (&["vscode"], folder!("vscode")),
    (&["claude"], folder!("claude")),
    (&["husky"], folder!("husky")),
    (
        &["public", "www", "wwwroot", "web", "website", "site"],
        folder!("public"),
    ),
    (
        &["assets", "asset", "static", "resources", "resource", "res"],
        folder!("resource"),
    ),
    (
        &[
            "images",
            "image",
            "img",
            "imgs",
            "icons",
            "icon",
            "screenshots",
            "pictures",
            "photos",
            "figures",
        ],
        folder!("images"),
    ),
    (&["fonts", "font"], folder!("font")),
    (&["svg", "svgs"], folder!("svg")),
    (&["video", "videos", "movies", "media"], folder!("video")),
    (&["audio", "sounds", "music"], folder!("audio")),
    (
        &["components", "component", "widgets", "widget"],
        folder!("components"),
    ),
    (
        &[
            "docs",
            "doc",
            "documentation",
            "documents",
            "wiki",
            "notes",
            "blog",
            "posts",
            "articles",
        ],
        folder!("docs"),
    ),
    (&["scripts", "script", "xtask"], folder!("scripts")),
    (
        &["tools", "tooling", "toolkit", "devtools"],
        folder!("tools"),
    ),
    (
        &[
            "config",
            "configs",
            "configuration",
            "conf",
            "cfg",
            "settings",
            "preferences",
        ],
        folder!("config"),
    ),
    (
        &[
            "dist", "build", "builds", "out", "output", "release", "bin", "compiled",
        ],
        folder!("dist"),
    ),
    (&["target"], folder!("target")),
    (&["migrations", "migration"], folder!("migrations")),
    (&["seeders", "seeds", "seed"], folder!("seeders")),
    (&["supabase"], folder!("supabase")),
    (&["terraform"], folder!("terraform")),
    (
        &["server", "servers", "backend", "infra", "infrastructure"],
        folder!("server"),
    ),
    (&["client", "clients", "frontend"], folder!("client")),
    (
        &["db", "database", "databases", "data", "sql"],
        folder!("database"),
    ),
    (
        &[
            "lib",
            "libs",
            "library",
            "libraries",
            "vendor",
            "vendors",
            "third-party",
            "third_party",
            "thirdparty",
            "external",
            "crates",
        ],
        folder!("lib"),
    ),
    (&["utils", "util", "utilities", "utility"], folder!("utils")),
    (&["helpers", "helper"], folder!("helper")),
    (&["api", "apis"], folder!("api")),
    (&["app", "apps", "application"], folder!("app")),
    (&["hooks", "hook"], folder!("hook")),
    (&["layouts", "layout"], folder!("layout")),
    (
        &["routes", "router", "routers", "routing", "navigation"],
        folder!("routes"),
    ),
    (
        &[
            "views", "view", "pages", "page", "screens", "screen", "html",
        ],
        folder!("views"),
    ),
    (&["styles", "style", "css", "stylesheets"], folder!("css")),
    (&["sass", "scss"], folder!("sass")),
    (
        &["types", "typings", "@types", "ts", "typescript"],
        folder!("typescript"),
    ),
    (&["js", "javascript"], folder!("javascript")),
    (&["json"], folder!("json")),
    (
        &["functions", "function", "lambda", "lambdas", "composables"],
        folder!("functions"),
    ),
    (&["middleware", "middlewares"], folder!("middleware")),
    (
        &[
            "controllers",
            "controller",
            "services",
            "service",
            "providers",
            "provider",
            "handlers",
            "handler",
        ],
        folder!("controller"),
    ),
    (
        &["models", "model", "schemas", "schema", "classes", "class"],
        folder!("class"),
    ),
    (&["store", "stores"], folder!("store")),
    (&["context", "contexts"], folder!("context")),
    (
        &["constants", "constant", "consts", "const"],
        folder!("constant"),
    ),
    (&["interfaces", "interface"], folder!("interface")),
    (
        &["env", "envs", "environment", "environments", "venv"],
        folder!("environment"),
    ),
    (&["ci"], folder!("ci")),
    (&["docker", "dockerfiles"], folder!("docker")),
    (
        &["devcontainer", "container", "containers"],
        folder!("container"),
    ),
    (&["kubernetes", "k8s"], folder!("kubernetes")),
    (&["cloudflare"], folder!("cloudflare")),
    (&["vercel"], folder!("vercel")),
    (&["next"], folder!("next")),
    (&["svelte", "svelte-kit"], folder!("svelte")),
    (&["turbo"], folder!("turborepo")),
    (&["yarn"], folder!("yarn")),
    (&["serverless"], folder!("serverless")),
    (&["python", "pycache", "pytest_cache"], folder!("python")),
    (&["php"], folder!("php")),
    (&["rust", "cargo"], folder!("rust")),
    (&["java"], folder!("java")),
    (&["go", "golang"], folder!("go")),
    (&["android"], folder!("android")),
    (&["ios"], folder!("ios")),
    (&["macos", "mac", "osx"], folder!("macos")),
    (&["windows", "win", "win32"], folder!("windows")),
    (&["linux", "unix"], folder!("linux")),
    (&["mobile"], folder!("mobile")),
    (&["desktop"], folder!("desktop")),
    (&["admin", "admins"], folder!("admin")),
    (&["archive", "archives"], folder!("archive")),
    (&["backup", "backups", "bak"], folder!("backup")),
    (&["downloads", "download"], folder!("download")),
    (&["uploads", "upload"], folder!("upload")),
    (&["pdf", "pdfs"], folder!("pdf")),
    (&["markdown", "md"], folder!("markdown")),
    (&["content", "contents"], folder!("content")),
    (&["tmp", "temp", "cache", "cached"], folder!("temp")),
    (&["logs", "log", "logging"], folder!("log")),
    (&["coverage", "nyc_output"], folder!("coverage")),
    (
        &["mocks", "mock", "fixtures", "fixture", "drafts"],
        folder!("mock"),
    ),
    (
        &["examples", "example", "demo", "demos", "samples", "sample"],
        folder!("examples"),
    ),
    (&["templates", "template"], folder!("template")),
    (&["private"], folder!("private")),
    (
        &[
            "auth",
            "authentication",
            "security",
            "secure",
            "certs",
            "ssl",
        ],
        folder!("secure"),
    ),
    (&["keys", "key", "secrets", "tokens"], folder!("keys")),
    (&["shared", "common"], folder!("shared")),
    (&["core"], folder!("core")),
    (&["packages", "package", "pkg", "pkgs"], folder!("packages")),
    (
        &[
            "plugins",
            "plugin",
            "extensions",
            "extension",
            "addons",
            "modules",
            "module",
        ],
        folder!("plugin"),
    ),
    (
        &[
            "i18n",
            "l10n",
            "locales",
            "locale",
            "lang",
            "langs",
            "languages",
            "translations",
        ],
        folder!("i18n"),
    ),
    (
        &["mail", "mails", "email", "emails", "mailers"],
        folder!("mail"),
    ),
    (&["jobs", "job"], folder!("job")),
    (&["queue", "queues"], folder!("queue")),
    (&["events", "event"], folder!("event")),
    (&["commands", "command", "cmd", "cli"], folder!("command")),
    (&["prisma"], folder!("prisma")),
    (&["drizzle"], folder!("drizzle")),
    (&["graphql", "gql"], folder!("graphql")),
    (&["storybook", "stories"], folder!("storybook")),
    (
        &["benchmarks", "benchmark", "bench", "benches", "perf"],
        folder!("benchmark"),
    ),
    (&["features", "feature"], folder!("features")),
    (
        &["generated", "generators", "generator", "gen"],
        folder!("generator"),
    ),
    (&["prompts", "prompt"], folder!("prompts")),
    (&["skills", "skill"], folder!("skills")),
    (&["agents", "agent", "bots", "bot"], folder!("robot")),
    (
        &["rules", "rule", "validators", "validation"],
        folder!("rules"),
    ),
    (&["themes", "theme", "colors", "design"], folder!("theme")),
    (&["ui", "gui"], folder!("ui")),
    (&["errors", "error"], folder!("error")),
    (&["projects", "project"], folder!("project")),
    (&["tasks"], folder!("tasks")),
    (&["trash"], folder!("trash")),
    (&["misc", "other", "others", "extras"], folder!("other")),
    (
        &["integrations", "integration", "connections"],
        folder!("connection"),
    ),
    (&["animations", "animation"], folder!("animation")),
    (&["proto", "protos", "protobuf"], folder!("proto")),
    (&["shaders", "shader"], folder!("shader")),
    (&["pipelines", "pipeline"], folder!("pipe")),
    (
        &["sandbox", "playground", "playgrounds"],
        folder!("sandbox"),
    ),
    (
        &["repositories", "repository", "repos", "repo"],
        folder!("repository"),
    ),
    (&["debug"], folder!("debug")),
];

/// A folder icon that only applies inside a specific parent folder.
pub(super) struct NestedFolder {
    pub parent: &'static str,
    pub name: &'static str,
    pub icon: FolderIcon,
}

pub(super) const NESTED_FOLDERS: &[NestedFolder] = &[
    NestedFolder {
        parent: "github",
        name: "workflows",
        icon: folder!("gh-workflows"),
    },
    NestedFolder {
        parent: "github",
        name: "issue_template",
        icon: folder!("template"),
    },
];
