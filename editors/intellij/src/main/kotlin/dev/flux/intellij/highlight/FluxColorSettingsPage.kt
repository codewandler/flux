package dev.flux.intellij.highlight

import com.intellij.openapi.editor.colors.TextAttributesKey
import com.intellij.openapi.fileTypes.SyntaxHighlighter
import com.intellij.openapi.options.colors.AttributesDescriptor
import com.intellij.openapi.options.colors.ColorDescriptor
import com.intellij.openapi.options.colors.ColorSettingsPage
import dev.flux.intellij.FluxIcons
import javax.swing.Icon

/** Settings ▸ Editor ▸ Color Scheme ▸ Flux-Lang — lets you recolor each token kind and preview it. */
class FluxColorSettingsPage : ColorSettingsPage {
    override fun getIcon(): Icon = FluxIcons.FILE
    override fun getHighlighter(): SyntaxHighlighter = FluxSyntaxHighlighter()
    override fun getAdditionalHighlightingTagToDescriptorMap(): Map<String, TextAttributesKey>? = null
    override fun getAttributeDescriptors(): Array<AttributesDescriptor> = DESCRIPTORS
    override fun getColorDescriptors(): Array<ColorDescriptor> = ColorDescriptor.EMPTY_ARRAY
    override fun getDisplayName(): String = "Flux-Lang"

    override fun getDemoText(): String = """
        # route an inbound caller utterance to a handler
        flow route-call(utterance: String, caller_id: String) -> RouteResult
          ${'$'}extract = intent_extract(${'$'}utterance, schema: CallerSlots)
          ${'$'}context = booking_lookup(${'$'}caller_id)
          @effect(network)
          ${'$'}page = web_fetch("https://status.example.com")

          when ${'$'}extract.intent == "book_flight"
            ${'$'}slots = ${'$'}extract.slots
            assert ${'$'}slots.destination, "no destination found"
            confirm "Create booking?", risk: medium
              ${'$'}booking = booking_create(${'$'}slots, caller: ${'$'}caller_id)
              return { intent: ${'$'}extract.intent, escalated: false }
          else
            repeat 3
              until ${'$'}done
              ${'$'}done = poll("queue", limit: 50, ratio: 1.5)
            return "escalated"
    """.trimIndent()

    companion object {
        private val DESCRIPTORS = arrayOf(
            AttributesDescriptor("Comment", FluxSyntaxHighlighter.COMMENT),
            AttributesDescriptor("Keyword", FluxSyntaxHighlighter.KEYWORD),
            AttributesDescriptor("Symbol (\$name)", FluxSyntaxHighlighter.SYMBOL),
            AttributesDescriptor("String", FluxSyntaxHighlighter.STRING),
            AttributesDescriptor("Number", FluxSyntaxHighlighter.NUMBER),
            AttributesDescriptor("Constant (true/false/null)", FluxSyntaxHighlighter.CONSTANT),
            AttributesDescriptor("Annotation (@effect, @thing)", FluxSyntaxHighlighter.ANNOTATION),
            AttributesDescriptor("Type", FluxSyntaxHighlighter.TYPE),
            AttributesDescriptor("Identifier / op", FluxSyntaxHighlighter.IDENTIFIER),
            AttributesDescriptor("Operator", FluxSyntaxHighlighter.OPERATOR),
            AttributesDescriptor("Comma", FluxSyntaxHighlighter.COMMA),
            AttributesDescriptor("Dot", FluxSyntaxHighlighter.DOT),
            AttributesDescriptor("Parentheses", FluxSyntaxHighlighter.PAREN),
            AttributesDescriptor("Brackets", FluxSyntaxHighlighter.BRACKET),
            AttributesDescriptor("Braces", FluxSyntaxHighlighter.BRACE),
            AttributesDescriptor("Bad character", FluxSyntaxHighlighter.BAD_CHAR),
        )
    }
}
