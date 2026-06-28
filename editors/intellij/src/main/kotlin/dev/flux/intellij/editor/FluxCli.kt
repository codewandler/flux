package dev.flux.intellij.editor

import java.io.File
import java.util.concurrent.TimeUnit

/** Locates the `fluxlang` binary the validator shells out to. */
object FluxCli {
    data class Output(val exitCode: Int, val stdout: String, val stderr: String)

    /** Run `<bin> <arg>` feeding [stdin], capturing both streams concurrently (no pipe deadlock). */
    fun exec(bin: String, arg: String, stdin: String, timeoutSec: Long = 10): Output? = try {
        val p = ProcessBuilder(bin, arg).start()
        val out = StringBuilder()
        val err = StringBuilder()
        val tOut = Thread { p.inputStream.bufferedReader(Charsets.UTF_8).use { out.append(it.readText()) } }
        val tErr = Thread { p.errorStream.bufferedReader(Charsets.UTF_8).use { err.append(it.readText()) } }
        tOut.start(); tErr.start()
        p.outputStream.use { it.write(stdin.toByteArray(Charsets.UTF_8)); it.flush() }
        if (!p.waitFor(timeoutSec, TimeUnit.SECONDS)) {
            p.destroyForcibly()
            null
        } else {
            tOut.join(2000); tErr.join(2000)
            Output(p.exitValue(), out.toString(), err.toString())
        }
    } catch (e: Exception) {
        null
    }

    /** Resolve `fluxlang`: $FLUXLANG_BIN, then the project's target/{debug,release}, then $PATH. */
    fun resolve(projectBase: String?): String? {
        System.getenv("FLUXLANG_BIN")?.let { p ->
            val f = File(p)
            if (f.canExecute()) return f.absolutePath
        }
        if (projectBase != null) {
            for (rel in listOf("target/debug/fluxlang", "target/release/fluxlang")) {
                val f = File(projectBase, rel)
                if (f.canExecute()) return f.absolutePath
            }
        }
        val path = System.getenv("PATH") ?: return null
        for (dir in path.split(File.pathSeparator)) {
            if (dir.isBlank()) continue
            val f = File(dir, "fluxlang")
            if (f.canExecute()) return f.absolutePath
        }
        return null
    }
}
