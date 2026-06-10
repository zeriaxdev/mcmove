//! Live smoke test for the SFTP layer against any reachable OpenSSH server.
//!
//! cargo run -p mcmove-core --example sftp_smoke -- <host> <port> <user> <keyfile> <base-dir>
//!
//! Exercises connect/auth, mkdirs, put, get, read, write, listdir, rename,
//! download_dir, rm_rf, exists — everything sync/pull/playerdata build on.

use std::path::Path;

use mcmove_core::config::Profile;
use mcmove_core::progress::NoopReporter;
use mcmove_core::sftp::{join, Auth, Sftp};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [host, port, user, key, base] = args.as_slice() else {
        eprintln!("usage: sftp_smoke <host> <port> <user> <keyfile> <base-dir>");
        std::process::exit(2);
    };
    let profile = Profile {
        host: host.clone(),
        port: port.parse()?,
        username: user.clone(),
        ..Profile::default()
    };
    let sftp = Sftp::connect(&profile, Auth::KeyFile(key.clone())).await?;
    println!("✓ connected + authenticated");

    let base = base.trim_end_matches('/');
    let dir = format!("{base}/mcmove-smoke/nested/deep");
    sftp.mkdirs(&dir).await?;
    assert!(sftp.exists(&dir).await);
    println!("✓ mkdirs + exists");

    let local = tempfile::tempdir()?;
    let f1 = local.path().join("hello.txt");
    std::fs::write(&f1, b"hello from mcmove")?;
    let remote_file = join(&dir, "hello.txt");
    sftp.put(&f1, &remote_file).await?;
    assert_eq!(sftp.read(&remote_file).await?, b"hello from mcmove");
    println!("✓ put + read");

    sftp.write(&join(&dir, "direct.txt"), b"written directly")
        .await?;
    let names: Vec<String> = sftp
        .listdir(&dir)
        .await?
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(names.contains(&"hello.txt".to_string()) && names.contains(&"direct.txt".to_string()));
    println!("✓ write + listdir: {names:?}");

    let renamed = join(&dir, "renamed.txt");
    sftp.rename(&remote_file, &renamed).await?;
    assert!(sftp.exists(&renamed).await && !sftp.exists(&remote_file).await);
    println!("✓ rename");

    let back = local.path().join("back.txt");
    sftp.get(&renamed, &back).await?;
    assert_eq!(std::fs::read(&back)?, b"hello from mcmove");
    println!("✓ get");

    let dl = local.path().join("tree");
    sftp.download_dir(&format!("{base}/mcmove-smoke"), &dl, &NoopReporter)
        .await?;
    assert!(dl.join("nested/deep/renamed.txt").is_file());
    println!("✓ download_dir (recursive)");

    let up_remote = format!("{base}/mcmove-smoke-up");
    sftp.upload_dir(Path::new(&dl), &up_remote, &NoopReporter)
        .await?;
    assert_eq!(
        sftp.read(&format!("{up_remote}/nested/deep/renamed.txt"))
            .await?,
        b"hello from mcmove"
    );
    println!("✓ upload_dir (recursive)");

    for d in [format!("{base}/mcmove-smoke"), up_remote] {
        sftp.rm_rf(&d).await?;
        let leftovers = sftp.listdir(&d).await?;
        assert!(leftovers.is_empty(), "rm_rf left {leftovers:?}");
        sftp.session.remove_dir(&d).await?;
    }
    println!("✓ rm_rf");

    sftp.close().await;
    println!("\nALL SFTP PRIMITIVES OK");
    Ok(())
}
