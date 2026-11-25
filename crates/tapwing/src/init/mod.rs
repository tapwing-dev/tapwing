//! `tapwing init` — wires `tapwing_driver` and `lib/main_tapwing.dart` into a Flutter app.

mod detect;
mod diff;
mod entrypoint;
mod hints;
mod pubspec;

use anyhow::{Context, Result, bail};
use clap::Args;
use std::path::PathBuf;
use std::process::Command;

pub use detect::{DetectedApp, detect_flutter_app};
pub use entrypoint::{
    MainSignature, ZoneWrapper, detect_zone_wrappers, parse_main_signature, render_entrypoint,
    render_entrypoint_with_zones,
};
pub use pubspec::{DriverSource, PubspecEdit, edit_pubspec};

/// Default git repository URL used for the `tapwing_driver` dev dependency.
pub const DEFAULT_DRIVER_URL: &str = "https://github.com/tapwing-dev/tapwing.git";
/// Path to `tapwing_driver` inside the Tapwing repository.
pub const DEFAULT_DRIVER_SUBDIR: &str = "packages/tapwing_driver";
/// Default Flutter app entrypoint relative to the app directory.
pub const DEFAULT_TARGET: &str = "lib/main.dart";
/// Default generated Tapwing entrypoint relative to the app directory.
pub const DEFAULT_OUTPUT: &str = "lib/main_tapwing.dart";

/// Default git ref (`v<CARGO_PKG_VERSION>`) matching the current CLI release tag.
pub fn default_driver_ref() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// CLI arguments for `tapwing init`.
#[derive(Debug, Clone, Args)]
pub struct InitArgs {
    /// Flutter project or pub workspace directory (default: current directory).
    #[arg(default_value = ".")]
    pub project_dir: PathBuf,
    /// Flutter app directory inside a pub workspace (required when a workspace has multiple apps).
    #[arg(long)]
    pub app: Option<PathBuf>,
    /// App entrypoint file relative to the Flutter app directory (e.g. `lib/main_dev.dart`).
    #[arg(long, default_value = DEFAULT_TARGET)]
    pub target: PathBuf,
    /// Generated Tapwing entrypoint file relative to the Flutter app directory.
    #[arg(long, default_value = DEFAULT_OUTPUT)]
    pub output: PathBuf,
    /// Use a local `path:` dependency for `tapwing_driver` instead of git.
    #[arg(long, conflicts_with_all = ["driver_ref", "driver_url"])]
    pub driver_path: Option<PathBuf>,
    /// Override the git ref for `tapwing_driver` (default: `v<tapwing version>`).
    #[arg(long)]
    pub driver_ref: Option<String>,
    /// Override the git repository URL for `tapwing_driver`.
    #[arg(long)]
    pub driver_url: Option<String>,
    /// Print the planned changes as a unified diff without writing any files.
    #[arg(long)]
    pub dry_run: bool,
    /// Overwrite an existing generated entrypoint and/or `tapwing_driver` pubspec entry.
    #[arg(long)]
    pub force: bool,
    /// Run `flutter pub get` after updating `pubspec.yaml`.
    #[arg(long)]
    pub pub_get: bool,
}

/// Planned action for a single file managed by `tapwing init`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePlan {
    Create { path: PathBuf, rel_label: String, new_content: String },
    Update { path: PathBuf, rel_label: String, old_content: String, new_content: String },
    SkipAlreadyPresent { path: PathBuf, rel_label: String },
    Unchanged { path: PathBuf, rel_label: String },
}

impl FilePlan {
    pub fn has_changes(&self) -> bool {
        matches!(self, Self::Create { .. } | Self::Update { .. })
    }

    pub fn unified_diff(&self) -> Option<String> {
        match self {
            Self::Create { rel_label, new_content, .. } => {
                Some(diff::unified_diff("/dev/null", &format!("b/{rel_label}"), "", new_content))
            }
            Self::Update { rel_label, old_content, new_content, .. } => {
                Some(diff::unified_diff(&format!("a/{rel_label}"), &format!("b/{rel_label}"), old_content, new_content))
            }
            Self::SkipAlreadyPresent { .. } | Self::Unchanged { .. } => None,
        }
    }
}

/// Complete dry-run/apply plan produced by [`plan_init`].
#[derive(Debug, Clone)]
pub struct InitPlan {
    pub app: DetectedApp,
    pub driver_source: DriverSource,
    pub main_signature: MainSignature,
    pub zone_wrappers: Vec<ZoneWrapper>,
    pub pubspec: FilePlan,
    pub entrypoint: FilePlan,
    pub android_flavors: Option<Vec<String>>,
}

impl InitPlan {
    /// True when neither `pubspec.yaml` nor the Tapwing entrypoint requires any edit.
    pub fn is_already_initialized(&self) -> bool {
        !self.pubspec.has_changes() && !self.entrypoint.has_changes()
    }

    /// Renders all planned file changes as a single unified diff string.
    pub fn render_unified_diff(&self) -> String {
        let mut out = String::new();
        for part in [self.pubspec.unified_diff(), self.entrypoint.unified_diff()].into_iter().flatten() {
            if !part.is_empty() {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&part);
            }
        }
        out
    }

    /// Formats the post-init next steps for this project.
    pub fn next_steps(&self, ran_pub_get: bool) -> String {
        hints::format_next_steps(
            self.app.app_rel_display.as_deref(),
            &self.app.output_rel,
            ran_pub_get,
            self.android_flavors.as_deref(),
        )
    }

    /// Formats the zone-mismatch setup warning when zone-creating wrappers were detected.
    pub fn zone_warning(&self) -> Option<String> {
        hints::format_zone_warning(
            self.app.app_rel_display.as_deref(),
            &self.app.target_rel,
            &self.app.output_rel,
            &self.zone_wrappers,
        )
    }
}

/// Computes all changes `tapwing init` would make without writing anything to disk.
pub fn plan_init(args: &InitArgs) -> Result<InitPlan> {
    let app = detect::detect_flutter_app(&args.project_dir, args.app.as_deref(), &args.target, &args.output)?;

    let driver_source = match &args.driver_path {
        Some(path) => DriverSource::Path(pubspec::compute_relative_driver_path(&app.app_dir, path)?),
        None => DriverSource::Git {
            url: args.driver_url.clone().unwrap_or_else(|| DEFAULT_DRIVER_URL.to_string()),
            path: DEFAULT_DRIVER_SUBDIR.to_string(),
            git_ref: args.driver_ref.clone().unwrap_or_else(default_driver_ref),
        },
    };

    let pubspec_path = app.app_dir.join("pubspec.yaml");
    let pubspec_old =
        std::fs::read_to_string(&pubspec_path).with_context(|| format!("failed to read {}", pubspec_path.display()))?;
    let pubspec_label = prefixed_label(app.app_rel_display.as_deref(), "pubspec.yaml");
    let pubspec = match pubspec::edit_pubspec(&pubspec_old, &driver_source, args.force)? {
        PubspecEdit::Updated(new_content) => {
            FilePlan::Update { path: pubspec_path, rel_label: pubspec_label, old_content: pubspec_old, new_content }
        }
        PubspecEdit::AlreadyPresent => FilePlan::SkipAlreadyPresent { path: pubspec_path, rel_label: pubspec_label },
        PubspecEdit::Unchanged => FilePlan::Unchanged { path: pubspec_path, rel_label: pubspec_label },
    };

    let target_path = app.app_dir.join(&app.target_rel);
    let target_src =
        std::fs::read_to_string(&target_path).with_context(|| format!("failed to read {}", target_path.display()))?;
    let main_signature = entrypoint::parse_main_signature(&target_src);
    let zone_wrappers = entrypoint::detect_zone_wrappers(&target_src);
    let generated =
        entrypoint::render_entrypoint_with_zones(&app.target_rel, &app.output_rel, main_signature, &zone_wrappers);

    let output_path = app.app_dir.join(&app.output_rel);
    let output_rel_str = app.output_rel.to_string_lossy().replace('\\', "/");
    let output_label = prefixed_label(app.app_rel_display.as_deref(), &output_rel_str);
    let entrypoint = if output_path.exists() {
        let existing = std::fs::read_to_string(&output_path)
            .with_context(|| format!("failed to read {}", output_path.display()))?;
        if !args.force {
            FilePlan::SkipAlreadyPresent { path: output_path, rel_label: output_label }
        } else if existing == generated {
            FilePlan::Unchanged { path: output_path, rel_label: output_label }
        } else {
            FilePlan::Update {
                path: output_path,
                rel_label: output_label,
                old_content: existing,
                new_content: generated,
            }
        }
    } else {
        FilePlan::Create { path: output_path, rel_label: output_label, new_content: generated }
    };

    let android_flavors = hints::detect_android_flavors(&app.app_dir);

    Ok(InitPlan { app, driver_source, main_signature, zone_wrappers, pubspec, entrypoint, android_flavors })
}

/// Applies a computed [`InitPlan`] to disk (and optionally runs `flutter pub get`).
pub fn apply_plan(plan: &InitPlan, run_pub_get: bool) -> Result<()> {
    write_file_plan(&plan.pubspec)?;
    write_file_plan(&plan.entrypoint)?;

    if run_pub_get {
        let status = Command::new("flutter")
            .args(["pub", "get"])
            .current_dir(&plan.app.app_dir)
            .status()
            .with_context(|| format!("failed to run `flutter pub get` in {}", plan.app.app_dir.display()))?;
        if !status.success() {
            bail!("`flutter pub get` failed with exit code {:?}", status.code());
        }
    }
    Ok(())
}

/// Entry point invoked by `tapwing init`.
pub fn run(args: InitArgs) -> Result<()> {
    let plan = plan_init(&args)?;
    let app_label = plan.app.app_rel_display.as_deref().unwrap_or(&plan.app.package_name);

    if plan.is_already_initialized() {
        if args.dry_run {
            println!("Already initialized ({app_label}): no changes needed.");
            return Ok(());
        }
        println!(
            "Already initialized ({app_label}): pubspec.yaml already lists tapwing_driver and {} already exists.",
            plan.app.output_rel.display()
        );
        if !args.force {
            println!("Pass --force to overwrite.");
        }
        if args.pub_get {
            apply_plan(&plan, true)?;
        }
        return Ok(());
    }

    if args.dry_run {
        println!("Dry run for {app_label} (no files written):\n");
        let diff_text = plan.render_unified_diff();
        if !diff_text.is_empty() {
            print!("{diff_text}");
        }
        print_skipped_dry_run(&plan.pubspec);
        print_skipped_dry_run(&plan.entrypoint);
        if args.pub_get {
            println!("\nWould run: flutter pub get");
        }
        if let Some(warning) = plan.zone_warning() {
            println!("\n{warning}");
        }
        return Ok(());
    }

    apply_plan(&plan, args.pub_get)?;

    println!("Initialized Tapwing in {app_label}:");
    print_applied_status(&plan.pubspec);
    print_applied_status(&plan.entrypoint);
    if args.pub_get {
        println!("  ✓ Ran `flutter pub get`");
    }
    println!();
    print!("{}", plan.next_steps(args.pub_get));
    if let Some(warning) = plan.zone_warning() {
        println!("\n{warning}");
    }
    Ok(())
}

fn write_file_plan(plan: &FilePlan) -> Result<()> {
    match plan {
        FilePlan::Create { path, new_content, .. } => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory {}", parent.display()))?;
            }
            std::fs::write(path, new_content).with_context(|| format!("failed to write {}", path.display()))
        }
        FilePlan::Update { path, new_content, .. } => {
            std::fs::write(path, new_content).with_context(|| format!("failed to write {}", path.display()))
        }
        FilePlan::SkipAlreadyPresent { .. } | FilePlan::Unchanged { .. } => Ok(()),
    }
}

fn print_applied_status(plan: &FilePlan) {
    match plan {
        FilePlan::Create { rel_label, .. } => println!("  ✓ Created {rel_label}"),
        FilePlan::Update { rel_label, .. } => println!("  ✓ Updated {rel_label}"),
        FilePlan::SkipAlreadyPresent { rel_label, .. } => {
            println!("  • Skipped {rel_label} (already present; pass --force to overwrite)");
        }
        FilePlan::Unchanged { rel_label, .. } => println!("  • Unchanged {rel_label}"),
    }
}

fn print_skipped_dry_run(plan: &FilePlan) {
    if let FilePlan::SkipAlreadyPresent { rel_label, .. } = plan {
        println!("Skipped {rel_label} (already present; pass --force to overwrite)");
    }
}

fn prefixed_label(prefix: Option<&str>, rel: &str) -> String {
    match prefix {
        Some(dir) => format!("{dir}/{rel}"),
        None => rel.to_string(),
    }
}
