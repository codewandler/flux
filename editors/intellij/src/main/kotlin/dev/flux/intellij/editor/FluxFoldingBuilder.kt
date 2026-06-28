package dev.flux.intellij.editor

import com.intellij.lang.ASTNode
import com.intellij.lang.folding.FoldingBuilderEx
import com.intellij.lang.folding.FoldingDescriptor
import com.intellij.openapi.editor.Document
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.util.TextRange
import com.intellij.psi.PsiElement

/**
 * Folds indentation blocks (a header line whose following lines are more deeply indented), computed
 * straight from the document — no grammar needed. Nested regions are emitted; the platform handles nesting.
 */
class FluxFoldingBuilder : FoldingBuilderEx(), DumbAware {
    override fun buildFoldRegions(root: PsiElement, document: Document, quick: Boolean): Array<FoldingDescriptor> {
        val node = root.node ?: return FoldingDescriptor.EMPTY_ARRAY
        val text = document.charsSequence
        val lineCount = document.lineCount
        if (lineCount < 2) return FoldingDescriptor.EMPTY_ARRAY

        val indent = IntArray(lineCount)
        val blank = BooleanArray(lineCount)
        for (i in 0 until lineCount) {
            val s = document.getLineStartOffset(i)
            val e = document.getLineEndOffset(i)
            var p = s
            while (p < e && text[p] == ' ') p++
            blank[i] = p >= e || isBlankRest(text, p, e)
            indent[i] = p - s
        }

        val out = ArrayList<FoldingDescriptor>()
        for (i in 0 until lineCount) {
            if (blank[i]) continue
            var j = i + 1
            while (j < lineCount && blank[j]) j++
            if (j >= lineCount || indent[j] <= indent[i]) continue

            // Extend the block while deeper-indented (blank lines stay inside).
            var last = j
            var k = j
            while (k < lineCount) {
                if (blank[k]) { k++; continue }
                if (indent[k] > indent[i]) { last = k; k++ } else break
            }

            val foldStart = document.getLineEndOffset(i)
            val foldEnd = document.getLineEndOffset(last)
            if (foldEnd > foldStart && document.getLineNumber(foldEnd) > i) {
                out.add(FoldingDescriptor(node, TextRange(foldStart, foldEnd)))
            }
        }
        return out.toTypedArray()
    }

    override fun getPlaceholderText(node: ASTNode): String = " …"
    override fun isCollapsedByDefault(node: ASTNode): Boolean = false

    private fun isBlankRest(text: CharSequence, from: Int, to: Int): Boolean {
        var p = from
        while (p < to) {
            if (!text[p].isWhitespace()) return false
            p++
        }
        return true
    }
}
