// Dumps what Maestro 2.12.1 parses each flow file into, as JSON, so Tapwing's
// parser can be checked against it command by command.
//
// Usage: java -cp <harness.jar>:<maestro lib jars> MaestroIrDumpKt <fixtures-root> <out-dir>
// Output per flow: <out-dir>/<relative path>.json with either
//   {"commands": [MaestroCommand...]}  or  {"error": {"type": ..., "message": ...}}
// Absolute paths under <fixtures-root> are replaced with "<ROOT>".

import com.fasterxml.jackson.annotation.JsonInclude
import com.fasterxml.jackson.databind.MapperFeature
import com.fasterxml.jackson.databind.SerializationFeature
import com.fasterxml.jackson.databind.json.JsonMapper
import com.fasterxml.jackson.module.kotlin.kotlinModule
import maestro.orchestra.yaml.YamlCommandReader
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import kotlin.io.path.extension
import kotlin.io.path.isRegularFile

fun main(args: Array<String>) {
    val root: Path = Paths.get(args[0]).toAbsolutePath().normalize()
    val out: Path = Paths.get(args[1]).toAbsolutePath().normalize()
    val mapper = JsonMapper.builder()
        .addModule(kotlinModule())
        .serializationInclusion(JsonInclude.Include.NON_NULL)
        .enable(SerializationFeature.INDENT_OUTPUT)
        .enable(SerializationFeature.ORDER_MAP_ENTRIES_BY_KEYS)
        .enable(MapperFeature.SORT_PROPERTIES_ALPHABETICALLY)
        .disable(SerializationFeature.FAIL_ON_EMPTY_BEANS)
        .build()
    val files = Files.walk(root).use { s ->
        s.filter { it.isRegularFile() && (it.extension == "yaml" || it.extension == "yml") }.sorted().toList()
    }
    var ok = 0
    var failed = 0
    for (p in files) {
        val rel = root.relativize(p).toString()
        val result: Map<String, Any?> = try {
            val commands = YamlCommandReader.readCommands(p)
            ok++
            mapOf("commands" to commands)
        } catch (e: Throwable) {
            failed++
            mapOf("error" to mapOf("type" to e.javaClass.simpleName, "message" to (e.message ?: "")))
        }
        val json = try {
            mapper.writeValueAsString(result)
        } catch (e: Throwable) {
            mapper.writeValueAsString(mapOf("serializationError" to (e.message ?: e.javaClass.name)))
        }
        val target = out.resolve("$rel.json")
        Files.createDirectories(target.parent)
        Files.writeString(target, json.replace(root.toString(), "<ROOT>") + "\n")
    }
    println("parsed ok: $ok, errors: $failed, total: ${files.size}")
}
