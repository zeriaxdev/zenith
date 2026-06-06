//! Minecraft version manifest, asset/library downloading, and launching.
//!
//! This is intentionally pragmatic: it handles the modern (1.13+) and legacy
//! argument formats, evaluates library/argument rules for the current OS,
//! extracts old-style natives, and spawns `java`. It does not download a JRE —
//! it uses the `java` on PATH (override with `ZENITH_JAVA`).

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use zenith_core::log as logbus;
use zenith_core::{Account, Loader, VersionEntry};
use zenith_store::Paths;

// ---- running game (single instance) -------------------------------------
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
static GAME_PID: AtomicU32 = AtomicU32::new(0);
static GAME_RUNNING: AtomicBool = AtomicBool::new(false);

/// True while a game process is alive.
pub fn is_running() -> bool {
    GAME_RUNNING.load(Ordering::Relaxed)
}

/// Terminate the running game, if any (SIGTERM).
pub fn kill_running() {
    let pid = GAME_PID.load(Ordering::Relaxed);
    if pid != 0 {
        logbus::info(format!("Stopping game (pid {pid})…"));
        let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
    }
}

// ---- version detail -----------------------------------------------------
#[derive(Deserialize)]
struct VersionDetail {
    #[serde(rename = "mainClass")]
    main_class: String,
    #[serde(rename = "assetIndex")]
    asset_index: AssetIndexRef,
    downloads: Downloads,
    libraries: Vec<Library>,
    #[serde(default)]
    arguments: Option<Arguments>,
    #[serde(rename = "minecraftArguments", default)]
    minecraft_arguments: Option<String>,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Deserialize)]
struct AssetIndexRef {
    id: String,
    url: String,
}

#[derive(Deserialize)]
struct Downloads {
    client: Artifact,
}

#[derive(Deserialize)]
struct Library {
    name: String,
    #[serde(default)]
    downloads: Option<LibDownloads>,
    #[serde(default)]
    rules: Option<Vec<Rule>>,
    #[serde(default)]
    natives: Option<HashMap<String, String>>,
    #[serde(default)]
    extract: Option<Extract>,
    /// Maven base URL (Fabric/Quilt/Forge style libraries have this instead of
    /// an explicit `downloads.artifact`).
    #[serde(default)]
    url: Option<String>,
}

/// A loader's partial profile JSON (Fabric/Quilt), merged onto vanilla.
#[derive(Deserialize)]
struct LoaderProfile {
    #[serde(rename = "mainClass")]
    main_class: String,
    #[serde(default)]
    libraries: Vec<Library>,
    #[serde(default)]
    arguments: Option<Arguments>,
}

#[derive(Deserialize)]
struct FabricLoaderEntry {
    loader: FabricLoaderInfo,
}

#[derive(Deserialize)]
struct FabricLoaderInfo {
    version: String,
    #[serde(default)]
    stable: bool,
}

fn latest_loader_version(loader: Loader, mc: &str) -> Result<String> {
    let base = match loader {
        Loader::Fabric => "https://meta.fabricmc.net/v2",
        Loader::Quilt => "https://meta.quiltmc.org/v3",
        _ => bail!("no meta API for {}", loader.label()),
    };
    let list: Vec<FabricLoaderEntry> = ureq::get(&format!("{base}/versions/loader/{mc}"))
        .call()
        .with_context(|| format!("listing {} loader versions", loader.label()))?
        .into_json()?;
    // prefer the newest stable, else the newest overall (list is newest-first)
    let chosen = list
        .iter()
        .find(|e| e.loader.stable)
        .or_else(|| list.first())
        .ok_or_else(|| anyhow!("no {} loader for {mc}", loader.label()))?;
    Ok(chosen.loader.version.clone())
}

/// Fetch vanilla detail and, for Fabric/Quilt, merge the loader profile onto it.
fn merged_detail(version: &VersionEntry, loader: Loader) -> Result<VersionDetail> {
    let mut base: VersionDetail = ureq::get(&version.url)
        .call()
        .context("fetching version detail")?
        .into_json()?;

    match loader {
        Loader::Vanilla => {}
        Loader::Fabric | Loader::Quilt => {
            let lv = latest_loader_version(loader, &version.id)?;
            let meta = match loader {
                Loader::Fabric => "https://meta.fabricmc.net/v2",
                _ => "https://meta.quiltmc.org/v3",
            };
            let url = format!("{meta}/versions/loader/{}/{lv}/profile/json", version.id);
            logbus::info(format!("{} loader {lv}", loader.label()));
            let prof: LoaderProfile = ureq::get(&url)
                .call()
                .with_context(|| format!("fetching {} profile", loader.label()))?
                .into_json()?;

            // loader libraries go first so loader classes take precedence
            let mut libs = prof.libraries;
            libs.append(&mut base.libraries);
            base.libraries = libs;
            base.main_class = prof.main_class;
            if let Some(pa) = prof.arguments {
                let ba = base.arguments.get_or_insert(Arguments {
                    game: Vec::new(),
                    jvm: Vec::new(),
                });
                ba.jvm.extend(pa.jvm);
                ba.game.extend(pa.game);
            }
        }
        Loader::Forge | Loader::NeoForge => {
            bail!(
                "{} isn't supported yet (it needs the installer's processor step).",
                loader.label()
            );
        }
    }
    Ok(base)
}

#[derive(Deserialize)]
struct LibDownloads {
    #[serde(default)]
    artifact: Option<Artifact>,
    #[serde(default)]
    classifiers: Option<HashMap<String, Artifact>>,
}

#[derive(Deserialize, Clone)]
struct Artifact {
    #[serde(default)]
    path: Option<String>,
    url: String,
}

#[derive(Deserialize)]
struct Extract {
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Deserialize)]
struct Rule {
    action: String,
    #[serde(default)]
    os: Option<OsRule>,
    #[serde(default)]
    features: Option<HashMap<String, bool>>,
}

#[derive(Deserialize)]
struct OsRule {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arch: Option<String>,
}

#[derive(Deserialize)]
struct Arguments {
    #[serde(default)]
    game: Vec<Arg>,
    #[serde(default)]
    jvm: Vec<Arg>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Arg {
    Plain(String),
    Conditional { rules: Vec<Rule>, value: ArgValue },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ArgValue {
    One(String),
    Many(Vec<String>),
}

// ---- OS / rule evaluation ----------------------------------------------
fn os_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "osx",
        other => other, // "windows", "linux"
    }
}

fn os_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        "x86" => "x86",
        other => other,
    }
}

/// Evaluate Mojang's allow/disallow rule list. `features` are all considered
/// false (we don't enable demo mode or custom resolution).
fn rules_allow(rules: &[Rule]) -> bool {
    let mut allowed = rules.is_empty();
    for rule in rules {
        let mut matches = true;
        if let Some(os) = &rule.os {
            if let Some(name) = &os.name {
                if name != os_name() {
                    matches = false;
                }
            }
            if let Some(arch) = &os.arch {
                if arch != os_arch() {
                    matches = false;
                }
            }
        }
        if let Some(features) = &rule.features {
            // any required feature flag means "not us" (all our features off)
            if features.values().any(|v| *v) {
                matches = false;
            }
        }
        if matches {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

// ---- download helpers ---------------------------------------------------
fn download_to(url: &str, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut reader = ureq::get(url)
        .call()
        .with_context(|| format!("downloading {url}"))?
        .into_reader();
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    std::fs::write(dest, &buf)?;
    Ok(())
}

fn maven_path(name: &str) -> Result<String> {
    // group:artifact:version[:classifier] -> group/path/artifact/version/artifact-version[-classifier].jar
    let parts: Vec<&str> = name.split(':').collect();
    if parts.len() < 3 {
        bail!("bad library name: {name}");
    }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let classifier = parts.get(3).map(|c| format!("-{c}")).unwrap_or_default();
    Ok(format!(
        "{group}/{artifact}/{version}/{artifact}-{version}{classifier}.jar"
    ))
}

// ---- prepared launch ----------------------------------------------------
pub struct Prepared {
    classpath: Vec<PathBuf>,
    natives_dir: PathBuf,
    main_class: String,
    asset_index_id: String,
    assets_dir: PathBuf,
    version_id: String,
    version_type: String,
    arguments: Option<Arguments>,
    legacy_arguments: Option<String>,
}

/// Download everything needed for `version` and return a launch plan.
/// `log` receives coarse progress lines.
pub fn prepare(version: &VersionEntry, loader: Loader, paths: &Paths) -> Result<Prepared> {
    logbus::info(format!("Fetching {} ({}) metadata…", version.id, loader.label()));
    let detail = merged_detail(version, loader)?;

    let vdir = paths.version_dir(&version.id);
    std::fs::create_dir_all(&vdir)?;

    // client jar
    logbus::info("Downloading client jar…");
    let client_jar = vdir.join(format!("{}.jar", version.id));
    download_to(&detail.downloads.client.url, &client_jar)?;

    // libraries + natives
    let mut classpath = vec![client_jar.clone()];
    let natives_dir = paths.natives_dir(&version.id);
    std::fs::create_dir_all(&natives_dir)?;

    logbus::progress_start("Libraries", detail.libraries.len());
    for lib in detail.libraries.iter() {
        logbus::progress_inc();
        if let Some(rules) = &lib.rules {
            if !rules_allow(rules) {
                continue;
            }
        }

        // old-style natives: pick the classifier for this OS and extract it
        if let Some(natives) = &lib.natives {
            if let Some(classifier_tpl) = natives.get(os_name()) {
                let classifier = classifier_tpl.replace("${arch}", if os_arch() == "x86" { "32" } else { "64" });
                if let Some(art) = lib
                    .downloads
                    .as_ref()
                    .and_then(|d| d.classifiers.as_ref())
                    .and_then(|c| c.get(&classifier))
                {
                    let path = art
                        .path
                        .clone()
                        .unwrap_or(maven_path(&format!("{}:{classifier}", lib.name))?);
                    let dest = paths.libraries().join(&path);
                    download_to(&art.url, &dest)?;
                    extract_natives(&dest, &natives_dir, lib.extract.as_ref())?;
                }
            }
            continue;
        }

        // normal library (includes modern natives jars, which go on classpath)
        let (url, path) = if let Some(art) = lib.downloads.as_ref().and_then(|d| d.artifact.as_ref())
        {
            (
                art.url.clone(),
                art.path.clone().unwrap_or(maven_path(&lib.name)?),
            )
        } else if let Some(base) = &lib.url {
            // Fabric/Quilt style: maven base URL + derived path
            let path = maven_path(&lib.name)?;
            (format!("{base}{path}"), path)
        } else {
            continue;
        };
        let dest = paths.libraries().join(&path);
        download_to(&url, &dest)?;
        classpath.push(dest);
    }
    logbus::progress_finish();

    // assets
    logbus::info(format!("Downloading assets ({})…", detail.asset_index.id));
    download_assets(&detail.asset_index, paths)?;
    logbus::info("Assets ready.");

    Ok(Prepared {
        classpath,
        natives_dir,
        main_class: detail.main_class,
        asset_index_id: detail.asset_index.id.clone(),
        assets_dir: paths.assets(),
        version_id: version.id.clone(),
        version_type: if detail.kind.is_empty() { version.kind.clone() } else { detail.kind },
        arguments: detail.arguments,
        legacy_arguments: detail.minecraft_arguments,
    })
}

fn extract_natives(jar: &Path, dest: &Path, extract: Option<&Extract>) -> Result<()> {
    let file = std::fs::File::open(jar)?;
    let mut archive = zip::ZipArchive::new(file)?;
    'entry: for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if entry.is_dir() || name.starts_with("META-INF/") {
            continue;
        }
        if let Some(ex) = extract {
            for pat in &ex.exclude {
                if name.starts_with(pat) {
                    continue 'entry;
                }
            }
        }
        let out = dest.join(Path::new(&name).file_name().unwrap_or_default());
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        std::fs::write(out, buf)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct AssetIndex {
    objects: HashMap<String, AssetObject>,
}

#[derive(Deserialize)]
struct AssetObject {
    hash: String,
}

fn download_assets(index_ref: &AssetIndexRef, paths: &Paths) -> Result<()> {
    use std::sync::Mutex;

    let index_path = paths.assets().join("indexes").join(format!("{}.json", index_ref.id));
    download_to(&index_ref.url, &index_path)?;
    let index: AssetIndex = serde_json::from_slice(&std::fs::read(&index_path)?)?;

    let objects_dir = paths.assets().join("objects");

    // collect only the objects we still need
    let pending: Vec<String> = index
        .objects
        .values()
        .map(|o| o.hash.clone())
        .filter(|hash| !objects_dir.join(&hash[0..2]).join(hash).exists())
        .collect();

    let total = pending.len();
    if total == 0 {
        return Ok(());
    }
    logbus::progress_start("Assets", total);

    // assets are thousands of tiny files; download them with a worker pool
    let queue = Mutex::new(pending.into_iter());
    let workers = 24usize;

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let hash = {
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    match q.next() {
                        Some(h) => h,
                        None => break,
                    }
                };
                let sub = &hash[0..2];
                let dest = objects_dir.join(sub).join(&hash);
                let url = format!("https://resources.download.minecraft.net/{sub}/{hash}");
                if let Err(e) = download_to(&url, &dest) {
                    logbus::warn(format!("asset {hash} failed: {e}"));
                }
                logbus::progress_inc();
            });
        }
    });
    logbus::progress_finish();

    Ok(())
}

// ---- build command + launch --------------------------------------------
fn classpath_string(cp: &[PathBuf]) -> String {
    cp.iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(":")
}

fn substitute(template: &str, vars: &HashMap<&str, String>) -> String {
    let mut s = template.to_string();
    for (k, v) in vars {
        s = s.replace(&format!("${{{k}}}"), v);
    }
    s
}

pub fn launch(prepared: &Prepared, account: &Account, paths: &Paths) -> Result<()> {
    let java = std::env::var("ZENITH_JAVA").unwrap_or_else(|_| "java".into());
    let cp = classpath_string(&prepared.classpath);

    let vars: HashMap<&str, String> = HashMap::from([
        ("auth_player_name", account.username.clone()),
        ("version_name", prepared.version_id.clone()),
        ("game_directory", paths.root.to_string_lossy().to_string()),
        ("assets_root", prepared.assets_dir.to_string_lossy().to_string()),
        ("assets_index_name", prepared.asset_index_id.clone()),
        ("auth_uuid", account.uuid.clone()),
        ("auth_access_token", account.access_token.clone()),
        ("clientid", String::new()),
        ("auth_xuid", String::new()),
        ("user_type", account.user_type.clone()),
        ("version_type", prepared.version_type.clone()),
        ("natives_directory", prepared.natives_dir.to_string_lossy().to_string()),
        ("launcher_name", "zenith".into()),
        ("launcher_version", env!("CARGO_PKG_VERSION").into()),
        ("classpath", cp.clone()),
    ]);

    let mut args: Vec<String> = Vec::new();

    if let Some(arguments) = &prepared.arguments {
        // modern format
        for a in &arguments.jvm {
            push_arg(a, &vars, &mut args);
        }
        args.push(prepared.main_class.clone());
        for a in &arguments.game {
            push_arg(a, &vars, &mut args);
        }
    } else {
        // legacy format
        args.push(format!("-Djava.library.path={}", prepared.natives_dir.to_string_lossy()));
        args.push("-cp".into());
        args.push(cp.clone());
        args.push(prepared.main_class.clone());
        if let Some(legacy) = &prepared.legacy_arguments {
            for tok in legacy.split_whitespace() {
                args.push(substitute(tok, &vars));
            }
        }
    }

    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    logbus::info(format!(
        "Launching {} ({}) as {}…",
        prepared.version_id, prepared.main_class, account.username
    ));

    if GAME_RUNNING.load(Ordering::Relaxed) {
        bail!("A game is already running. Stop it first.");
    }

    let mut child = std::process::Command::new(&java)
        .args(&args)
        .current_dir(&paths.root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to launch `{java}` — is Java installed and on PATH?"))?;

    GAME_PID.store(child.id(), Ordering::Relaxed);
    GAME_RUNNING.store(true, Ordering::Relaxed);

    // stream the game's output into the console
    if let Some(out) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                logbus::game(line);
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                logbus::game(line);
            }
        });
    }
    // reap the process and report exit without blocking the caller
    std::thread::spawn(move || {
        match child.wait() {
            Ok(status) => logbus::info(format!("Game exited ({status}).")),
            Err(e) => logbus::error(format!("Failed to wait on game: {e}")),
        }
        GAME_RUNNING.store(false, Ordering::Relaxed);
        GAME_PID.store(0, Ordering::Relaxed);
    });

    Ok(())
}

fn push_arg(arg: &Arg, vars: &HashMap<&str, String>, out: &mut Vec<String>) {
    match arg {
        Arg::Plain(s) => out.push(substitute(s, vars)),
        Arg::Conditional { rules, value } => {
            if rules_allow(rules) {
                match value {
                    ArgValue::One(s) => out.push(substitute(s, vars)),
                    ArgValue::Many(v) => {
                        for s in v {
                            out.push(substitute(s, vars));
                        }
                    }
                }
            }
        }
    }
}
