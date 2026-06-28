package dev.flux.intellij.editor

import com.intellij.codeInsight.completion.CompletionContributor
import com.intellij.codeInsight.completion.CompletionParameters
import com.intellij.codeInsight.completion.CompletionProvider
import com.intellij.codeInsight.completion.CompletionResultSet
import com.intellij.codeInsight.completion.CompletionType
import com.intellij.codeInsight.lookup.LookupElementBuilder
import com.intellij.patterns.PlatformPatterns
import com.intellij.util.ProcessingContext
import dev.flux.intellij.FluxIcons
import dev.flux.intellij.FluxLanguage

/**
 * Basic completion for `.flux`: keywords, built-in types, host ops, `@annotations`, and the `$symbols`
 * already bound in the current file. Context-light by design — the flat PSI carries no scope, so we
 * offer the full vocabulary and let the prefix matcher filter.
 */
class FluxCompletionContributor : CompletionContributor() {
    init {
        extend(CompletionType.BASIC, PlatformPatterns.psiElement().withLanguage(FluxLanguage), Provider)
    }

    private object Provider : CompletionProvider<CompletionParameters>() {
        private val SYMBOL_RE = Regex("\\$[A-Za-z_][A-Za-z0-9_]*")

        override fun addCompletions(
            parameters: CompletionParameters,
            context: ProcessingContext,
            resultSet: CompletionResultSet,
        ) {
            val text = parameters.editor.document.charsSequence
            val caret = parameters.offset

            // Compute the real prefix ourselves so leading `$` / `@` are included.
            var start = caret
            while (start > 0 && isPrefixChar(text[start - 1])) start--
            val prefix = text.subSequence(start, caret).toString()
            val result = resultSet.withPrefixMatcher(prefix)

            for (k in FluxVocabulary.KEYWORDS) {
                result.addElement(LookupElementBuilder.create(k).bold().withTypeText("keyword"))
            }
            for (t in FluxVocabulary.TYPES) {
                result.addElement(LookupElementBuilder.create(t).withTypeText("type"))
            }
            for (op in FluxVocabulary.OPS) {
                result.addElement(LookupElementBuilder.create(op).withIcon(FluxIcons.FILE).withTypeText("op"))
            }
            for (a in FluxVocabulary.ANNOTATIONS) {
                result.addElement(LookupElementBuilder.create(a).withTypeText("annotation"))
            }

            // $symbols already bound in this file.
            val seen = HashSet<String>()
            for (m in SYMBOL_RE.findAll(text)) {
                if (seen.add(m.value)) {
                    result.addElement(LookupElementBuilder.create(m.value).withTypeText("symbol"))
                }
            }
        }

        private fun isPrefixChar(c: Char): Boolean =
            c == '$' || c == '@' || c == '_' || c.isLetterOrDigit()
    }
}
