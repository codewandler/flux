package dev.flux.intellij.lang

import com.intellij.lexer.LexerBase
import com.intellij.psi.TokenType
import com.intellij.psi.tree.IElementType

/**
 * Hand-written highlighting lexer for Flux-Lang. No JFlex/codegen step and no parser — it only needs to
 * classify each token for coloring, brace matching, and commenting. Each token is determined purely by
 * scanning forward from its start, so restarting at any token boundary (state 0) is always correct.
 *
 * Lexical rules mirror crates/flux-lang/docs/syntax.md and the keyword set in crates/flux-lang/src/parse.rs.
 */
class FluxLexer : LexerBase() {
    private var buffer: CharSequence = ""
    private var bufferEnd = 0
    private var tokenStart = 0
    private var tokenEnd = 0
    private var tokenType: IElementType? = null

    override fun start(buffer: CharSequence, startOffset: Int, endOffset: Int, initialState: Int) {
        this.buffer = buffer
        this.bufferEnd = endOffset
        this.tokenStart = startOffset
        this.tokenEnd = startOffset
        locateToken()
    }

    override fun getState(): Int = 0
    override fun getTokenType(): IElementType? = tokenType
    override fun getTokenStart(): Int = tokenStart
    override fun getTokenEnd(): Int = tokenEnd
    override fun getBufferSequence(): CharSequence = buffer
    override fun getBufferEnd(): Int = bufferEnd

    override fun advance() {
        tokenStart = tokenEnd
        locateToken()
    }

    private fun locateToken() {
        if (tokenStart >= bufferEnd) {
            tokenType = null
            tokenEnd = tokenStart
            return
        }
        when (val c = buffer[tokenStart]) {
            ' ', '\t', '\r', '\n' -> lexWhitespace()
            '#' -> lexComment()
            '"' -> lexString()
            '$' -> lexSigil(FluxTokens.SYMBOL)
            '@' -> lexSigil(FluxTokens.ANNOTATION)
            in '0'..'9' -> lexNumber()
            else -> if (isIdentStart(c)) lexIdentifier() else lexOperator()
        }
    }

    private fun lexWhitespace() {
        var i = tokenStart + 1
        while (i < bufferEnd && isWhitespace(buffer[i])) i++
        tokenEnd = i
        tokenType = TokenType.WHITE_SPACE
    }

    private fun lexComment() {
        var i = tokenStart + 1
        while (i < bufferEnd && buffer[i] != '\n') i++
        tokenEnd = i
        tokenType = FluxTokens.COMMENT
    }

    private fun lexString() {
        if (hasAt(tokenStart, "\"\"\"")) {
            // Triple-quoted: spans newlines; `#` inside is literal; ends at the next `"""` or EOF.
            var i = tokenStart + 3
            while (i < bufferEnd) {
                if (hasAt(i, "\"\"\"")) {
                    i += 3
                    break
                }
                i++
            }
            tokenEnd = minOf(i, bufferEnd)
            tokenType = FluxTokens.STRING
            return
        }
        var i = tokenStart + 1
        while (i < bufferEnd) {
            when (buffer[i]) {
                '\\' -> i += 2          // skip the escaped char
                '"' -> { i++; break }   // closing quote
                '\n' -> break           // unterminated single-line string ends at EOL
                else -> i++
            }
        }
        tokenEnd = minOf(i, bufferEnd)
        tokenType = FluxTokens.STRING
    }

    /** `$name` (symbol) or `@name` (effect/thing annotation). A bare sigil is a bad character. */
    private fun lexSigil(type: IElementType) {
        var i = tokenStart + 1
        if (i < bufferEnd && isIdentStart(buffer[i])) {
            i++
            while (i < bufferEnd && isIdentPart(buffer[i])) i++
            tokenEnd = i
            tokenType = type
        } else {
            tokenEnd = tokenStart + 1
            tokenType = TokenType.BAD_CHARACTER
        }
    }

    private fun lexNumber() {
        var i = tokenStart + 1
        while (i < bufferEnd && buffer[i].isDigit()) i++
        if (i + 1 < bufferEnd && buffer[i] == '.' && buffer[i + 1].isDigit()) {
            i += 2
            while (i < bufferEnd && buffer[i].isDigit()) i++
        }
        tokenEnd = i
        tokenType = FluxTokens.NUMBER
    }

    private fun lexIdentifier() {
        var i = tokenStart + 1
        while (i < bufferEnd && isIdentPart(buffer[i])) i++
        tokenEnd = i
        val text = buffer.subSequence(tokenStart, i).toString()
        tokenType = when {
            KEYWORDS.contains(text) -> FluxTokens.KEYWORD
            CONSTANTS.contains(text) -> FluxTokens.CONSTANT
            // Symbols are `$lower`, ops/flows are lowercase; a Capitalized bare word is a type.
            BUILTIN_TYPES.contains(text) || text[0].isUpperCase() -> FluxTokens.TYPE
            else -> FluxTokens.IDENTIFIER
        }
    }

    private fun lexOperator() {
        val c = buffer[tokenStart]
        if (tokenStart + 1 < bufferEnd) {
            val two = "" + c + buffer[tokenStart + 1]
            if (TWO_CHAR_OPS.contains(two)) {
                tokenEnd = tokenStart + 2
                tokenType = FluxTokens.OPERATOR
                return
            }
        }
        tokenEnd = tokenStart + 1
        tokenType = when (c) {
            '(' -> FluxTokens.LPAREN
            ')' -> FluxTokens.RPAREN
            '[' -> FluxTokens.LBRACKET
            ']' -> FluxTokens.RBRACKET
            '{' -> FluxTokens.LBRACE
            '}' -> FluxTokens.RBRACE
            ',' -> FluxTokens.COMMA
            '.' -> FluxTokens.DOT
            '=', '<', '>', '+', '-', '*', '/', '!', '|', ':', '&', '?' -> FluxTokens.OPERATOR
            else -> TokenType.BAD_CHARACTER
        }
    }

    private fun hasAt(pos: Int, s: String): Boolean {
        if (pos + s.length > bufferEnd) return false
        for (k in s.indices) if (buffer[pos + k] != s[k]) return false
        return true
    }

    private fun isWhitespace(c: Char) = c == ' ' || c == '\t' || c == '\r' || c == '\n'
    private fun isIdentStart(c: Char) = c.isLetter() || c == '_'
    private fun isIdentPart(c: Char) = c.isLetterOrDigit() || c == '_'

    companion object {
        // Structural / control-flow keywords. Deliberately NOT included: effect names like
        // `read`/`delete`/`model`/`network` — those are common op names and must stay plain identifiers.
        private val KEYWORDS: Set<String> = hashSetOf(
            "flow", "type", "goal",
            "when", "unless", "else",
            "repeat", "until", "each", "in", "flat",
            "loop", "watch", "timeout", "budget", "for", "every",
            "match", "case", "default", "route", "fallback", "branch",
            "block", "seq", "pipe",
            "parallel", "race",
            "try", "catch", "retry",
            "confirm", "throttle", "debounce",
            "assert", "verify",
            "memo", "await", "as", "peek",
            "return", "do",
        )
        private val CONSTANTS: Set<String> = hashSetOf("true", "false", "null")
        private val BUILTIN_TYPES: Set<String> = hashSetOf("String", "Number", "Bool", "Any", "List")
        private val TWO_CHAR_OPS: Set<String> = hashSetOf("->", "==", "!=", "<=", ">=", "&&", "||", "+=")
    }
}
