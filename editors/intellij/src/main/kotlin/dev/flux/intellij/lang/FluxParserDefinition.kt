package dev.flux.intellij.lang

import com.intellij.extapi.psi.ASTWrapperPsiElement
import com.intellij.lang.ASTNode
import com.intellij.lang.ParserDefinition
import com.intellij.lang.PsiParser
import com.intellij.lexer.Lexer
import com.intellij.openapi.project.Project
import com.intellij.psi.FileViewProvider
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiFile
import com.intellij.psi.TokenType
import com.intellij.psi.tree.IFileElementType
import com.intellij.psi.tree.TokenSet
import dev.flux.intellij.FluxLanguage

/**
 * A deliberately flat PSI: the parser consumes every token as a leaf under one file node. We do not
 * re-implement the Flux-Lang grammar in Kotlin (the Rust `fluxlang` owns semantics) — this PSI exists
 * only so the platform will host editor features (completion, folding, external validation, structure
 * view) that key off a real PSI file for the language.
 */
class FluxParserDefinition : ParserDefinition {
    override fun createLexer(project: Project?): Lexer = FluxLexer()
    override fun createParser(project: Project?): PsiParser = FluxParser()
    override fun getFileNodeType(): IFileElementType = FILE
    override fun getCommentTokens(): TokenSet = COMMENTS
    override fun getStringLiteralElements(): TokenSet = STRINGS
    override fun getWhitespaceTokens(): TokenSet = WHITESPACE
    override fun createElement(node: ASTNode): PsiElement = ASTWrapperPsiElement(node)
    override fun createFile(viewProvider: FileViewProvider): PsiFile = FluxFile(viewProvider)

    companion object {
        @JvmField val FILE = IFileElementType(FluxLanguage)
        val COMMENTS: TokenSet = TokenSet.create(FluxTokens.COMMENT)
        val STRINGS: TokenSet = TokenSet.create(FluxTokens.STRING)
        val WHITESPACE: TokenSet = TokenSet.create(TokenType.WHITE_SPACE)
    }
}
