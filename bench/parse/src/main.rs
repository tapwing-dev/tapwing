// Parse-only benchmark of tapwing::flow; counterpart of MaestroParseBench.kt.
// Run both with bench/parse/run_parse_bench.sh.
//   tapwing-parse-bench <fixtures-root> [iterations] [warmup]
//   tapwing-parse-bench --once <flow.yaml>   # fresh process: parse one flow, exit
use std::path::{Path, PathBuf};
use std::time::Instant;

fn yaml_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            yaml_files(&p, out);
        } else if matches!(p.extension().and_then(|e| e.to_str()), Some("yaml" | "yml")) {
            out.push(p);
        }
    }
}

fn parse_one(p: &Path) -> bool {
    std::hint::black_box(tapwing::flow::parse_file(p)).is_ok()
}

fn pct(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q) as usize]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args[1] == "--once" {
        if !parse_one(Path::new(&args[2])) {
            eprintln!("parse failed");
            std::process::exit(1);
        }
        return;
    }
    let t0 = Instant::now();
    let root = std::fs::canonicalize(&args[1]).unwrap();
    let iters: usize = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(30);
    let warm: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(10);
    let mut files = Vec::new();
    yaml_files(&root, &mut files);
    let c0 = Instant::now();
    parse_one(&files[0]);
    let c1 = Instant::now();
    let f0 = Instant::now();
    let ok = files.iter().filter(|p| parse_one(p)).count();
    let f1 = Instant::now();
    for _ in 0..warm {
        for p in &files {
            parse_one(p);
        }
    }
    let mut per_file = vec![vec![0f64; iters]; files.len()];
    let mut totals = vec![0f64; iters];
    for i in 0..iters {
        let a = Instant::now();
        for (j, p) in files.iter().enumerate() {
            let s = Instant::now();
            parse_one(p);
            per_file[j][i] = s.elapsed().as_secs_f64() * 1e6;
        }
        totals[i] = a.elapsed().as_secs_f64() * 1e3;
    }
    let mut med: Vec<f64> = per_file
        .into_iter()
        .map(|mut v| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[iters / 2]
        })
        .collect();
    med.sort_by(|a, b| a.partial_cmp(b).unwrap());
    totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("files={} ok={}", files.len(), ok);
    println!("main_to_first_parse_ms={:.3}", (c1 - t0).as_secs_f64() * 1e3);
    println!("first_file_cold_ms={:.3}", (c1 - c0).as_secs_f64() * 1e3);
    println!("first_full_pass_ms={:.2}", (f1 - f0).as_secs_f64() * 1e3);
    println!("warm_full_pass_ms median={:.2} p10={:.2} p90={:.2}", pct(&totals, 0.5), pct(&totals, 0.1), pct(&totals, 0.9));
    println!(
        "warm_per_file_us median={:.1} p90={:.1} p99={:.1} max={:.1}",
        pct(&med, 0.5), pct(&med, 0.9), pct(&med, 0.99), med[med.len() - 1]
    );
}
