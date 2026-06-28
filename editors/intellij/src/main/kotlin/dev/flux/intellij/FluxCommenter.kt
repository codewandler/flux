package dev.flux.intellij

import com.intellij.lang.Commenter

/** Flux-Lang has only `#` line comments — no block comments (see crates/flux-lang/docs/syntax.md). */
class FluxCommenter : Commenter {
    override fun getLineCommentPrefix(): String = "#"
    override fun getBlockCommentPrefix(): String? = null
    override fun getBlockCommentSuffix(): String? = null
    override fun getCommentedBlockCommentPrefix(): String? = null
    override fun getCommentedBlockCommentSuffix(): String? = null
}
