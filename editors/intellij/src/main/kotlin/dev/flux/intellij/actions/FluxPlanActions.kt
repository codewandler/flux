package dev.flux.intellij.actions

import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.CommonDataKeys
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.openapi.ui.Messages
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.components.JBTextArea
import dev.flux.intellij.FluxFileType
import dev.flux.intellij.editor.FluxCli
import java.awt.Dimension
import java.awt.Font
import javax.swing.JComponent

/**
 * Preview a `.flux` file by reusing the Rust toolchain: compile the text to its JSON AST (`fluxlang
 * compile`) and optionally render it to the box-drawing plan tree (`fluxlang render`). JSON-wire-form
 * files are rendered directly. Showcases flux's "see the plan before it runs" idea inside the editor.
 */
abstract class FluxPlanActionBase(private val asTree: Boolean) : AnAction() {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        val file = e.getData(CommonDataKeys.PSI_FILE)
        e.presentation.isEnabledAndVisible = file?.fileType == FluxFileType.INSTANCE
    }

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val psi = e.getData(CommonDataKeys.PSI_FILE) ?: return
        if (psi.fileType != FluxFileType.INSTANCE) return
        val text = psi.viewProvider.document?.text ?: psi.text

        val bin = FluxCli.resolve(project.basePath)
        if (bin == null) {
            Messages.showErrorDialog(
                project,
                "Could not find the `fluxlang` binary.\n\nTried \$FLUXLANG_BIN, " +
                    "<project>/target/{debug,release}/fluxlang, and \$PATH.\n" +
                    "Build it with: cargo build -p flux-lang --features cli --bin fluxlang",
                "Flux-Lang",
            )
            return
        }

        val outcome = ProgressManager.getInstance()
            .runProcessWithProgressSynchronously<Outcome, RuntimeException>(
                { compute(bin, text) }, "Running fluxlang…", true, project,
            )

        if (outcome.error != null) {
            Messages.showErrorDialog(project, outcome.error, "Flux-Lang")
            return
        }
        TextDialog(project, if (asTree) "Flux Plan" else "Flux AST (JSON)", outcome.output ?: "").show()
    }

    private data class Outcome(val output: String?, val error: String?)

    private fun compute(bin: String, text: String): Outcome {
        val json: String
        if (looksLikeJson(text)) {
            json = text
        } else {
            val compiled = FluxCli.exec(bin, "compile", text)
                ?: return Outcome(null, "fluxlang timed out or failed to start.")
            if (compiled.exitCode != 0) return Outcome(null, clean(compiled.stderr))
            json = compiled.stdout
        }
        if (!asTree) return Outcome(json, null)

        val rendered = FluxCli.exec(bin, "render", json)
            ?: return Outcome(null, "fluxlang timed out or failed to start.")
        if (rendered.exitCode != 0) return Outcome(null, clean(rendered.stderr))
        return Outcome(rendered.stdout, null)
    }

    private fun clean(stderr: String): String =
        stderr.trim().removePrefix("fluxlang:").trim().ifBlank { "fluxlang failed." }

    private fun looksLikeJson(text: String): Boolean {
        for (line in text.lineSequence()) {
            val t = line.trim()
            if (t.isEmpty() || t.startsWith("#")) continue
            return t.startsWith("{") || t.startsWith("[")
        }
        return false
    }
}

class FluxShowPlanAction : FluxPlanActionBase(asTree = true)

class FluxShowAstAction : FluxPlanActionBase(asTree = false)

private class TextDialog(project: Project, dialogTitle: String, private val content: String) :
    DialogWrapper(project, true) {
    init {
        title = dialogTitle
        init()
    }

    override fun createCenterPanel(): JComponent {
        val area = JBTextArea(content).apply {
            isEditable = false
            font = Font(Font.MONOSPACED, Font.PLAIN, 13)
            caretPosition = 0
        }
        return JBScrollPane(area).apply { preferredSize = Dimension(760, 520) }
    }

    override fun createActions() = arrayOf(okAction)
}
