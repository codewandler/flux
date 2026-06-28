package dev.flux.intellij

import com.intellij.lang.BracePair
import com.intellij.lang.PairedBraceMatcher
import com.intellij.psi.PsiFile
import com.intellij.psi.tree.IElementType
import dev.flux.intellij.lang.FluxTokens

/** Matches `()`, `[]`, `{}` using the highlighting lexer's split left/right tokens (no PSI needed). */
class FluxBraceMatcher : PairedBraceMatcher {
    override fun getPairs(): Array<BracePair> = PAIRS

    override fun isPairedBracesAllowedBeforeType(lbraceType: IElementType, contextType: IElementType?): Boolean = true

    override fun getCodeConstructStart(file: PsiFile?, openingBraceOffset: Int): Int = openingBraceOffset

    companion object {
        private val PAIRS = arrayOf(
            BracePair(FluxTokens.LPAREN, FluxTokens.RPAREN, false),
            BracePair(FluxTokens.LBRACKET, FluxTokens.RBRACKET, false),
            BracePair(FluxTokens.LBRACE, FluxTokens.RBRACE, false),
        )
    }
}
