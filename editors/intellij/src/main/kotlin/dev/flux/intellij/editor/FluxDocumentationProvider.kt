package dev.flux.intellij.editor

import com.intellij.lang.documentation.AbstractDocumentationProvider
import com.intellij.openapi.editor.Editor
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiFile

/** Quick-doc (Ctrl-Q) and hover for Flux-Lang keywords/nodes, from [FluxVocabulary.DOCS]. */
class FluxDocumentationProvider : AbstractDocumentationProvider() {
    override fun getQuickNavigateInfo(element: PsiElement?, originalElement: PsiElement?): String? =
        render(originalElement ?: element)

    override fun generateDoc(element: PsiElement?, originalElement: PsiElement?): String? {
        val body = render(originalElement ?: element) ?: return null
        return "<html><body>$body</body></html>"
    }

    /** Treat the token under the caret as the documentation target (the flat PSI has no references). */
    override fun getCustomDocumentationElement(
        editor: Editor,
        file: PsiFile,
        contextElement: PsiElement?,
        targetOffset: Int,
    ): PsiElement? {
        val word = contextElement?.text?.trim() ?: return null
        return if (FluxVocabulary.DOCS.containsKey(word)) contextElement else null
    }

    private fun render(el: PsiElement?): String? {
        val word = el?.text?.trim() ?: return null
        val doc = FluxVocabulary.DOCS[word] ?: return null
        return "<b>$word</b> — $doc"
    }
}
