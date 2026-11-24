// Dumps what Maestro 2.12.1's WorkspaceExecutionPlanner decides for each case
// in compat/workspace-cases.json, so Tapwing's planner can be checked against it.
//
// Usage: java -cp <harness.jar>:<maestro lib jars> WorkspacePlanDumpKt <workspaces-root> <cases.json> <out.json>
// Output: {"<case>": {"flowsToRun": [...], "sequence": [...], "continueOnFailure": bool}
//                 or {"error": {"type": ..., "message": ...}}}
// Paths are relative to <workspaces-root>; in messages it is replaced with "<ROOT>".
// Inputs are made absolute first, like TestCommand does.

import com.fasterxml.jackson.databind.SerializationFeature
import com.fasterxml.jackson.databind.json.JsonMapper
import com.fasterxml.jackson.module.kotlin.kotlinModule
import maestro.orchestra.workspace.WorkspaceExecutionPlanner
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths

fun main(args: Array<String>) {
    val root: Path = Paths.get(args[0]).toAbsolutePath().normalize()
    val mapper = JsonMapper.builder().addModule(kotlinModule()).enable(SerializationFeature.INDENT_OUTPUT).build()
    val cases: List<Map<String, Any?>> = mapper.readValue(Paths.get(args[1]).toFile(), List::class.java)
        .map { @Suppress("UNCHECKED_CAST") (it as Map<String, Any?>) }
    fun rel(p: Path): String = root.relativize(p.toAbsolutePath().normalize()).toString()
    val out = linkedMapOf<String, Any?>()
    for (case in cases) {
        val name = case["name"] as String
        @Suppress("UNCHECKED_CAST")
        val input = (case["input"] as List<String>).map { root.resolve(it) }.toSet()
        @Suppress("UNCHECKED_CAST")
        val include = case["includeTags"] as List<String>
        @Suppress("UNCHECKED_CAST")
        val exclude = case["excludeTags"] as List<String>
        val config = (case["config"] as String?)?.let { root.resolve(it) }
        out[name] = try {
            val plan = WorkspaceExecutionPlanner.plan(input, include, exclude, config)
            linkedMapOf(
                "flowsToRun" to plan.flowsToRun.map(::rel).sorted(),
                "sequence" to plan.sequence.flows.map(::rel),
                "continueOnFailure" to (plan.sequence.continueOnFailure == true),
            )
        } catch (e: Exception) {
            mapOf("error" to mapOf(
                "type" to e.javaClass.simpleName,
                "message" to (e.message ?: "").replace(root.toString(), "<ROOT>"),
            ))
        }
    }
    Files.writeString(Paths.get(args[2]), mapper.writeValueAsString(out) + "\n")
}
