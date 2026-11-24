// Parse-only benchmark of Maestro's YamlCommandReader; counterpart of src/main.rs.
// Run both with bench/parse/run_parse_bench.sh.
//   java -cp <bench.jar>:<maestro lib jars> MaestroParseBenchKt <fixtures-root> [iterations] [warmup]
//   java -cp ... MaestroParseBenchKt --once <flow.yaml>   # fresh process: parse one flow, exit
// Prints the cold first parse, the first full pass, then warm per-pass totals and per-file
// medians. Rejected flows count too: the parsing work still happens.

import maestro.orchestra.yaml.YamlCommandReader
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import kotlin.io.path.extension
import kotlin.io.path.isRegularFile

fun parseOne(p: Path): Boolean = try {
    YamlCommandReader.readCommands(p); true
} catch (e: Throwable) { false }

fun pct(sorted: List<Double>, q: Double): Double = sorted[((sorted.size - 1) * q).toInt()]

fun main(args: Array<String>) {
    if (args[0] == "--once") { // cold process: parse one file and exit
        val ok = parseOne(Paths.get(args[1]))
        if (!ok) { System.err.println("parse failed"); kotlin.system.exitProcess(1) }
        return
    }
    val t0 = System.nanoTime()
    val root: Path = Paths.get(args[0]).toAbsolutePath().normalize()
    val iters = args.getOrNull(1)?.toInt() ?: 30
    val warm = args.getOrNull(2)?.toInt() ?: 10
    val files = Files.walk(root).use { s ->
        s.filter { it.isRegularFile() && (it.extension == "yaml" || it.extension == "yml") }.sorted().toList()
    }
    // Cold: first file parsed in a fresh JVM (class loading + Jackson/Kotlin module init).
    val c0 = System.nanoTime(); parseOne(files[0]); val c1 = System.nanoTime()
    // First full pass (still mostly cold JIT).
    val f0 = System.nanoTime(); var ok = 0; for (p in files) if (parseOne(p)) ok++; val f1 = System.nanoTime()
    repeat(warm) { for (p in files) parseOne(p) }
    val perFile = Array(files.size) { DoubleArray(iters) }
    val totals = DoubleArray(iters)
    for (i in 0 until iters) {
        val a = System.nanoTime()
        for ((j, p) in files.withIndex()) {
            val s = System.nanoTime(); parseOne(p); perFile[j][i] = (System.nanoTime() - s) / 1e3
        }
        totals[i] = (System.nanoTime() - a) / 1e6
    }
    val medPerFile = perFile.map { it.sorted()[iters / 2] }.sorted()
    val tot = totals.sorted()
    println("files=${files.size} ok=$ok")
    println("jvm_main_to_first_parse_ms=%.1f".format((c1 - t0) / 1e6))
    println("first_file_cold_ms=%.1f".format((c1 - c0) / 1e6))
    println("first_full_pass_ms=%.1f".format((f1 - f0) / 1e6))
    println("warm_full_pass_ms median=%.2f p10=%.2f p90=%.2f".format(pct(tot, 0.5), pct(tot, 0.1), pct(tot, 0.9)))
    println("warm_per_file_us median=%.1f p90=%.1f p99=%.1f max=%.1f".format(
        pct(medPerFile, 0.5), pct(medPerFile, 0.9), pct(medPerFile, 0.99), medPerFile.last()))
}
