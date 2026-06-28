package dev.flux.intellij.highlight

import com.intellij.openapi.editor.DefaultLanguageHighlighterColors
import com.intellij.openapi.editor.HighlighterColors
import com.intellij.openapi.editor.colors.TextAttributesKey
import com.intellij.openapi.editor.colors.TextAttributesKey.createTextAttributesKey
import com.intellij.openapi.fileTypes.SyntaxHighlighterBase
import com.intellij.lexer.Lexer
import com.intellij.psi.TokenType
import com.intellij.psi.tree.IElementType
import dev.flux.intellij.lang.FluxLexer
import dev.flux.intellij.lang.FluxTokens

class FluxSyntaxHighlighter : SyntaxHighlighterBase() {
    override fun getHighlightingLexer(): Lexer = FluxLexer()

    override fun getTokenHighlights(tokenType: IElementType): Array<TextAttributesKey> = when (tokenType) {
        FluxTokens.COMMENT -> COMMENT_KEYS
        FluxTokens.KEYWORD -> KEYWORD_KEYS
        FluxTokens.SYMBOL -> SYMBOL_KEYS
        FluxTokens.STRING -> STRING_KEYS
        FluxTokens.NUMBER -> NUMBER_KEYS
        FluxTokens.CONSTANT -> CONSTANT_KEYS
        FluxTokens.ANNOTATION -> ANNOTATION_KEYS
        FluxTokens.TYPE -> TYPE_KEYS
        FluxTokens.IDENTIFIER -> IDENTIFIER_KEYS
        FluxTokens.OPERATOR -> OPERATOR_KEYS
        FluxTokens.COMMA -> COMMA_KEYS
        FluxTokens.DOT -> DOT_KEYS
        FluxTokens.LPAREN, FluxTokens.RPAREN -> PAREN_KEYS
        FluxTokens.LBRACKET, FluxTokens.RBRACKET -> BRACKET_KEYS
        FluxTokens.LBRACE, FluxTokens.RBRACE -> BRACE_KEYS
        TokenType.BAD_CHARACTER -> BAD_KEYS
        else -> EMPTY
    }

    companion object {
        val COMMENT = key("FLUX_COMMENT", DefaultLanguageHighlighterColors.LINE_COMMENT)
        val KEYWORD = key("FLUX_KEYWORD", DefaultLanguageHighlighterColors.KEYWORD)
        val SYMBOL = key("FLUX_SYMBOL", DefaultLanguageHighlighterColors.INSTANCE_FIELD)
        val STRING = key("FLUX_STRING", DefaultLanguageHighlighterColors.STRING)
        val NUMBER = key("FLUX_NUMBER", DefaultLanguageHighlighterColors.NUMBER)
        val CONSTANT = key("FLUX_CONSTANT", DefaultLanguageHighlighterColors.CONSTANT)
        val ANNOTATION = key("FLUX_ANNOTATION", DefaultLanguageHighlighterColors.METADATA)
        val TYPE = key("FLUX_TYPE", DefaultLanguageHighlighterColors.CLASS_NAME)
        val IDENTIFIER = key("FLUX_IDENTIFIER", DefaultLanguageHighlighterColors.IDENTIFIER)
        val OPERATOR = key("FLUX_OPERATOR", DefaultLanguageHighlighterColors.OPERATION_SIGN)
        val COMMA = key("FLUX_COMMA", DefaultLanguageHighlighterColors.COMMA)
        val DOT = key("FLUX_DOT", DefaultLanguageHighlighterColors.DOT)
        val PAREN = key("FLUX_PAREN", DefaultLanguageHighlighterColors.PARENTHESES)
        val BRACKET = key("FLUX_BRACKET", DefaultLanguageHighlighterColors.BRACKETS)
        val BRACE = key("FLUX_BRACE", DefaultLanguageHighlighterColors.BRACES)
        val BAD_CHAR = key("FLUX_BAD_CHARACTER", HighlighterColors.BAD_CHARACTER)

        private fun key(name: String, fallback: TextAttributesKey) = createTextAttributesKey(name, fallback)
        private fun keys(k: TextAttributesKey) = arrayOf(k)

        private val EMPTY = TextAttributesKey.EMPTY_ARRAY
        private val COMMENT_KEYS = keys(COMMENT)
        private val KEYWORD_KEYS = keys(KEYWORD)
        private val SYMBOL_KEYS = keys(SYMBOL)
        private val STRING_KEYS = keys(STRING)
        private val NUMBER_KEYS = keys(NUMBER)
        private val CONSTANT_KEYS = keys(CONSTANT)
        private val ANNOTATION_KEYS = keys(ANNOTATION)
        private val TYPE_KEYS = keys(TYPE)
        private val IDENTIFIER_KEYS = keys(IDENTIFIER)
        private val OPERATOR_KEYS = keys(OPERATOR)
        private val COMMA_KEYS = keys(COMMA)
        private val DOT_KEYS = keys(DOT)
        private val PAREN_KEYS = keys(PAREN)
        private val BRACKET_KEYS = keys(BRACKET)
        private val BRACE_KEYS = keys(BRACE)
        private val BAD_KEYS = keys(BAD_CHAR)
    }
}
