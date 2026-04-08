#[allow(non_snake_case)]
use clap::Parser;
use clap::CommandFactory;
use clap_complete::{generate, Shell};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::io;
use std::path::PathBuf;
use tokio::fs;

#[derive(Parser)]
#[command(about = "Minecraft metadata downloader")]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
    json_file: Option<String>,
    #[arg(default_value = "minecraft")]
    base_path: String,
    #[arg(long)]
    version: Option<String>,
    #[arg(long)]
    fetch: bool,
    #[arg(long)]
    fetch_manifest: bool,
    // Only is used when --fetch_manifest is set, to determine which manifest to fetch
    #[arg(long)]
    v1: bool,
    #[arg(long)]
    release_type: Option<ReleaseType>,
}

#[derive(Parser)]
enum Commands {
    /// Generate shell completions
    Completion {
        /// Shell type for completions
        #[arg(value_enum)]
        shell: Shell,
    },
}

async fn download_file(
    url: &str,
    dest: &PathBuf,
    sha1: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = reqwest::get(url).await?;
    let bytes = response.bytes().await?;
    fs::create_dir_all(dest.parent().unwrap()).await?;
    fs::write(dest, &bytes).await?;
    if let Some(expected_sha1) = sha1 {
        println!("Verifying SHA1 for {:?}", dest);
        let bytes_clone = bytes.clone();
        let computed = tokio::task::spawn_blocking(move || {
            let mut hasher = Sha1::new();
            hasher.update(&bytes_clone);
            format!("{:x}", hasher.finalize())
        })
        .await?;
        println!("Successfully verified SHA1 for {:?}", dest);
        if computed != expected_sha1 {
            println!("SHA1 mismatch for {:?}", dest);
            fs::remove_file(dest).await?;
            return Err("SHA1 mismatch".into());
        }
    }
    Ok(())
}

fn process_json(
    data: &Value,
    base_path: &str,
    version: &str,
) -> Vec<(String, PathBuf, Option<String>)> {
    let mut downloads = vec![];
    fn recurse(
        v: &Value,
        base_path: &str,
        version: &str,
        downloads: &mut Vec<(String, PathBuf, Option<String>)>,
    ) {
        match v {
            Value::Object(map) => {
                if let Some(url) = map.get("url") {
                    if let Some(url_str) = url.as_str() {
                        let sha1 = map
                            .get("sha1")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string());
                        let dest = if let Some(path) = map.get("path") {
                            // libraries
                            PathBuf::from(base_path)
                                .join("libraries")
                                .join(path.as_str().unwrap())
                        } else if url_str.contains("launchermeta.mojang.com") {
                            // asset index
                            PathBuf::from(base_path)
                                .join("assets")
                                .join("indexes")
                                .join(format!("{}.json", version))
                        } else if url_str.contains("launcher.mojang.com")
                            && url_str.contains("client.jar")
                        {
                            // client jar
                            PathBuf::from(base_path)
                                .join("versions")
                                .join(version)
                                .join(format!("{}.jar", version))
                        } else if url_str.contains("launcher.mojang.com")
                            && url_str.contains("server.jar")
                        {
                            // server jar
                            PathBuf::from(base_path)
                                .join("versions")
                                .join(version)
                                .join(format!("{}-server.jar", version))
                        } else if url_str.contains("launcher.mojang.com")
                            && url_str.contains(".xml")
                        {
                            // logging config
                            PathBuf::from(base_path)
                                .join("assets")
                                .join("log_configs")
                                .join(format!("client-{}.xml", version))
                        } else {
                            println!("Unknown URL: {}", url_str);
                            return;
                        };
                        downloads.push((url_str.to_string(), dest, sha1));
                    }
                } else {
                    for value in map.values() {
                        recurse(value, base_path, version, downloads);
                    }
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    recurse(item, base_path, version, downloads);
                }
            }
            _ => {}
        }
    }
    recurse(data, base_path, version, &mut downloads);
    downloads
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_url: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
    let manifest_url_v1: &str = "https://piston-meta.mojang.com/mc/game/version_manifest.json";

    let args: Args = Args::parse();

    // Handle completion command
    if let Some(Commands::Completion { shell }) = args.command {
        let mut cmd = Args::command();
        let name = cmd.get_name().to_string();
        generate(shell, &mut cmd, name, &mut io::stdout());
        return Ok(());
    }

    let json_content: String = if let Some(json_file) = &args.json_file {
        tokio::fs::read_to_string(json_file).await?
    } else {
        let manifest: Value = getManifest(manifest_url).await;
        let version: &str = args
            .version
            .as_deref()
            .unwrap_or_else(|| manifest["latest"]["release"].as_str().unwrap());
        let versions: &Vec<Value> = manifest["versions"].as_array().unwrap();
        let version_data: &Value = versions
            .iter()
            .find(|v: &&Value| v["id"] == version)
            .ok_or("Version not found")?;
        let version_url: &str = version_data["url"].as_str().unwrap();
        if args.fetch_manifest {
            if args.v1 {                
                println!("Fetching version manifest v1 from {}", manifest_url_v1);
            } else {
                println!("Fetching version manifest v2 from {}", manifest_url);
            }
        } else {
            println!("Version {} found at {}", version, version_url);
        }
        reqwest::get(version_url).await?.text().await?
    };

    if args.fetch {
        let manifest: Value = getManifest(manifest_url).await;

        let version_str: String = manifest["latest"]["release"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        fs::write(format!("{}.json", version_str), &json_content).await?;
        return Ok(());
    }

    if args.fetch_manifest {
        if args.v1 {
            let json_file: String =
                reqwest::get("https://piston-meta.mojang.com/mc/game/version_manifest.json")
                    .await?
                    .text()
                    .await?;
            tokio::fs::write("version_manifest.json", &json_file).await?;
            return Ok(());
        }

        let json_file: String =
            reqwest::get("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")
                .await?
                .text()
                .await?;
        tokio::fs::write("version_manifest_v2.json", &json_file).await?;
        return Ok(());
    }

    let data: Value = serde_json::from_str(&json_content)?;
    let version: &str = data
        .get("id")
        .and_then(|v: &Value| v.as_str())
        .unwrap_or("unknown");
    let downloads: Vec<(String, PathBuf, Option<String>)> =
        process_json(&data, &args.base_path, version);
    for (url, dest, sha1) in downloads {
        println!("Downloading {} to {:?}", url, dest);
        if let Err(e) = download_file(&url, &dest, sha1.as_deref()).await {
            eprintln!("Failed to download {}: {}", url, e);
        }
    }
    Ok(())
}

#[allow(non_snake_case)]
async fn getManifest(manifest_url: &str) -> Value {
    let manifest_text: String = reqwest::get(manifest_url)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let manifest: Value = serde_json::from_str(&manifest_text).unwrap();
    manifest
}

#[allow(non_snake_case)]
#[derive(clap::ValueEnum, Clone)]
enum ReleaseType {
    Release,
    Snapshot,
}
