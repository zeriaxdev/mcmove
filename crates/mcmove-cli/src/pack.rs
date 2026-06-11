//! `mcmove pack` — CLI front-end for the patcher in `mcmove_core::pack`.
//! All printing and prompting happens here; the core stays silent.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use mcmove_core::modrinth;
use mcmove_core::pack::{self, ModEntry, Plan};

use crate::color::{bold, green, red, yellow};
use crate::report::CliReporter;

pub async fn create(instance: &Path, out: Option<PathBuf>) -> anyhow::Result<()> {
    let client = modrinth::client()?;
    let reporter = CliReporter::new();
    let entries = pack::scan_mods(instance, &client, &reporter).await?;

    let out_path = out.unwrap_or_else(|| default_patch_name(instance));
    let man = pack::write_bundle(&entries, &out_path, &instance_name(instance))?;
    let n_modrinth = man.mods.iter().filter(|m| m.source == "modrinth").count();
    let size = fs::metadata(&out_path)?.len() as f64 / (1024.0 * 1024.0);
    println!(
        "\nWrote {} ({size:.1} MB): {} mods, {n_modrinth} Modrinth, {} bundled",
        out_path.display(),
        man.mods.len(),
        man.mods.len() - n_modrinth,
    );
    println!(
        "Send {} and the executable to your friend.",
        file_name(&out_path)
    );
    Ok(())
}

pub async fn share(instance: &Path, bin: Option<String>, filename: String) -> anyhow::Result<()> {
    let bin = bin.unwrap_or_else(pack::random_code);
    if !pack::valid_bin(&bin) {
        bail!("bin code must use only letters, numbers, dash, or underscore");
    }
    if filename.is_empty() || filename.contains('/') || filename.contains('\\') {
        bail!("invalid filename");
    }
    let client = modrinth::client()?;
    let reporter = CliReporter::new();
    let entries = pack::scan_mods(instance, &client, &reporter).await?;

    let tmp = tempfile::tempdir()?;
    let patch_path = tmp.path().join(&filename);
    let man = pack::write_bundle(&entries, &patch_path, &instance_name(instance))?;

    let n_bundled = man.mods.iter().filter(|m| m.source == "bundled").count();
    if n_bundled > 0 {
        println!("\nNote: {n_bundled} off-Modrinth jar(s) are inside this public upload.");
        if !confirm("Upload anyway?", true) {
            println!("aborted");
            return Ok(());
        }
    }
    let url = pack::upload_filebin(&client, &bin, &filename, &patch_path).await?;
    println!("\nUploaded patch.");
    println!("Short code: {bin}");
    println!("Full link : {url}");
    println!("\nFriend runs:");
    println!("  mcmove.exe pack apply {bin} \"C:\\Path\\To\\Instance\"");
    Ok(())
}

pub async fn apply(
    patch: &str,
    instance: &Path,
    dry_run: bool,
    keep_extra: bool,
    yes: bool,
) -> anyhow::Result<()> {
    if !instance.is_dir() {
        bail!("not a folder: {}", instance.display());
    }
    let mods_dir = instance.join("mods");
    fs::create_dir_all(&mods_dir).context("creating mods/")?;

    let client = modrinth::client()?;
    let reporter = CliReporter::new();
    let (patch_path, _tmp) = pack::resolve_patch_source(patch, &client, &reporter).await?;
    let (mut archive, man) = pack::load_bundle(&patch_path)?;

    let current: Vec<ModEntry> = if has_jars(&mods_dir) {
        pack::scan_mods(instance, &client, &reporter).await?
    } else {
        Vec::new()
    };
    let plan = pack::plan_apply(&man.mods, &current, keep_extra);
    print_plan(&plan, keep_extra);
    if plan.is_noop() {
        println!("\nAlready matched. Nothing to do.");
        return Ok(());
    }
    if dry_run {
        println!("\n(dry run - no changes made)");
        return Ok(());
    }
    if !yes
        && !confirm(
            &format!("\nApply this patch to {}?", instance.display()),
            true,
        )
    {
        println!("aborted");
        return Ok(());
    }
    pack::execute_plan(&mut archive, &plan, &mods_dir, &client, &reporter).await?;
    println!(
        "{}",
        green(bold("\nDone. Restart Minecraft so the new mod set loads.").as_str())
    );
    Ok(())
}

fn print_plan(plan: &Plan, keep_extra: bool) {
    println!("\n--- current/mods");
    println!("+++ patch/mods");
    for e in &plan.add {
        println!("{}", green(&format!("+ {} [{}]", e.filename, e.source)));
    }
    for (old, new) in &plan.update {
        println!(
            "{}",
            yellow(&format!("~ {} -> {}", old.filename, new.filename))
        );
    }
    for e in &plan.remove {
        println!("{}", red(&format!("- {}", e.filename)));
    }
    if keep_extra {
        println!("# extra local mods will be kept");
    }
    println!(
        "\nPlan: add {}, update {}, remove {}, unchanged {}",
        plan.add.len(),
        plan.update.len(),
        plan.remove.len(),
        plan.keep.len()
    );
}

fn confirm(prompt: &str, default: bool) -> bool {
    print!("{prompt} ({}): ", if default { "Y/n" } else { "y/N" });
    io::stdout().flush().ok();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return default;
    }
    let line = line.trim().to_ascii_lowercase();
    match line.as_str() {
        "" => default,
        "y" | "yes" => true,
        _ => false,
    }
}

fn has_jars(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("jar"))
        })
    })
}

fn instance_name(instance: &Path) -> String {
    let name = file_name(instance);
    if name.is_empty() {
        "mods".into()
    } else {
        name
    }
}

fn default_patch_name(instance: &Path) -> PathBuf {
    PathBuf::from(format!("{}.mcmpatch", instance_name(instance)))
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
