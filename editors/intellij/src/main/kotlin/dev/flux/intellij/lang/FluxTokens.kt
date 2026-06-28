package dev.flux.intellij.lang

/** Token kinds emitted by [FluxLexer]. Highlighting-only — there is no PSI/parser in Tier 1. */
object FluxTokens {
    @JvmField val COMMENT = FluxTokenType("COMMENT")
    @JvmField val KEYWORD = FluxTokenType("KEYWORD")
    @JvmField val SYMBOL = FluxTokenType("SYMBOL")
    @JvmField val STRING = FluxTokenType("STRING")
    @JvmField val NUMBER = FluxTokenType("NUMBER")
    @JvmField val CONSTANT = FluxTokenType("CONSTANT")
    @JvmField val ANNOTATION = FluxTokenType("ANNOTATION")
    @JvmField val TYPE = FluxTokenType("TYPE")
    @JvmField val IDENTIFIER = FluxTokenType("IDENTIFIER")
    @JvmField val OPERATOR = FluxTokenType("OPERATOR")
    @JvmField val COMMA = FluxTokenType("COMMA")
    @JvmField val DOT = FluxTokenType("DOT")

    // Split left/right so the brace matcher can pair them; highlighting treats each pair alike.
    @JvmField val LPAREN = FluxTokenType("LPAREN")
    @JvmField val RPAREN = FluxTokenType("RPAREN")
    @JvmField val LBRACKET = FluxTokenType("LBRACKET")
    @JvmField val RBRACKET = FluxTokenType("RBRACKET")
    @JvmField val LBRACE = FluxTokenType("LBRACE")
    @JvmField val RBRACE = FluxTokenType("RBRACE")
}
