package dev.flux.intellij.editor

import com.intellij.lang.annotation.AnnotationHolder
import com.intellij.lang.annotation.ExternalAnnotator
import com.intellij.lang.annotation.HighlightSeverity
import com.intellij.openapi.util.TextRange
import com.intellij.psi.PsiFile
import dev.flux.intellij.FluxFileType
import java.util.concurrent.TimeUnit

/**
 * Live validation by reusing the real Rust parser: pipe the buffer to `fluxlang compile` (text form) or
 * `fluxlang render` (JSON wire form) and surface a non-zero exit as an error annotation.
 *
 * Flux-Lang errors carry no source spans, so we position the squiggle best-effort: on the line named in
 * the message (errors often quote the offending line in backticks), else on the first meaningful line.
 */
class FluxExternalAnnotator : ExternalAnnotator<FluxExternalAnnotator.Info, FluxExternalAnnotator.Result>() {

    data class Info(val text: String, val bin: String?)
    data class Result(val message: String, val start: Int, val end: Int)

    override fun collectInformation(file: PsiFile): Info? {
        if (file.fileType != FluxFileType.INSTANCE) return null
        val text = file.text
        if (text.isBlank() || text.length > MAX_LEN) return null
        return Info(text, FluxCli.resolve(file.project.basePath))
    }

    override fun doAnnotate(info: Info): Result? {
        val bin = info.bin ?: return null // no binary available → skip silently
        val mode = if (looksLikeJson(info.text)) "render" else "compile"
        val (exit, stderr) = runFluxlang(bin, mode, info.text) ?: return null
        if (exit == 0) return null
        val message = cleanMessage(stderr).ifBlank { "fluxlang $mode failed" }
        val (start, end) = locate(info.text, message)
        return Result(message, start, end)
    }

    override fun apply(file: PsiFile, result: Result?, holder: AnnotationHolder) {
        result ?: return
        val max = file.textLength
        val start = result.start.coerceIn(0, max)
        val end = result.end.coerceIn(start, max)
        val range = if (end > start) TextRange(start, end) else TextRange(0, minOf(1, max))
        holder.newAnnotation(HighlightSeverity.ERROR, result.message)
            .range(range)
            .create()
    }

    private fun runFluxlang(bin: String, mode: String, stdin: String): Pair<Int, String>? = try {
        val pb = ProcessBuilder(bin, mode)
        pb.redirectOutput(ProcessBuilder.Redirect.DISCARD) // we only care about exit + stderr
        val p = pb.start()
        p.outputStream.use { it.write(stdin.toByteArray(Charsets.UTF_8)); it.flush() }
        val err = p.errorStream.bufferedReader(Charsets.UTF_8).readText()
        if (!p.waitFor(8, TimeUnit.SECONDS)) {
            p.destroyForcibly()
            null
        } else {
            p.exitValue() to err
        }
    } catch (e: Exception) {
        null
    }

    /** `fluxlang: parse error: parse error: …` → `parse error: …`. */
    private fun cleanMessage(stderr: String): String =
        stderr.trim()
            .removePrefix("fluxlang:").trim()
            .replace("parse error: parse error:", "parse error:")

    /** Best-effort source range for an error with no span. */
    private fun locate(text: String, message: String): Pair<Int, Int> {
        // 1) errors that quote the offending line in backticks (text form).
        BACKTICK.find(message)?.groupValues?.get(1)?.let { snippet ->
            if (snippet.isNotBlank()) {
                val idx = text.indexOf(snippet)
                if (idx >= 0) return lineRange(text, idx)
            }
        }
        // 2) JSON deserialization errors carry `line N column M`.
        LINE_COL.find(message)?.let { mt ->
            val ln = mt.groupValues[1].toIntOrNull()
            val col = mt.groupValues[2].toIntOrNull()
            if (ln != null && col != null) {
                offsetOf(text, ln, col)?.let { return lineRange(text, it) }
            }
        }
        // 3) Fall back to the first non-blank, non-comment line.
        var i = 0
        for (line in text.lineSequence()) {
            val t = line.trim()
            if (t.isNotEmpty() && !t.startsWith("#")) {
                val lead = line.indexOfFirst { !it.isWhitespace() }.coerceAtLeast(0)
                return (i + lead) to (i + line.length)
            }
            i += line.length + 1
        }
        return 0 to minOf(1, text.length)
    }

    /** The [start, end) offsets of the line containing [offset]. */
    private fun lineRange(text: String, offset: Int): Pair<Int, Int> {
        var s = offset
        while (s > 0 && text[s - 1] != '\n') s--
        var e = offset
        while (e < text.length && text[e] != '\n') e++
        val lead = (s until e).firstOrNull { !text[it].isWhitespace() } ?: s
        return lead to e
    }

    /** Offset of 1-based [line1]/[col1] in [text], or null if out of range. */
    private fun offsetOf(text: String, line1: Int, col1: Int): Int? {
        if (line1 < 1 || col1 < 1) return null
        var off = 0
        var ln = 1
        while (ln < line1) {
            val nl = text.indexOf('\n', off)
            if (nl < 0) return null
            off = nl + 1
            ln++
        }
        return (off + (col1 - 1)).coerceAtMost(text.length)
    }

    private fun looksLikeJson(text: String): Boolean {
        for (line in text.lineSequence()) {
            val t = line.trim()
            if (t.isEmpty() || t.startsWith("#")) continue
            return t.startsWith("{") || t.startsWith("[")
        }
        return false
    }

    companion object {
        private const val MAX_LEN = 200_000
        private val BACKTICK = Regex("`([^`]+)`")
        private val LINE_COL = Regex("line (\\d+) column (\\d+)")
    }
}
