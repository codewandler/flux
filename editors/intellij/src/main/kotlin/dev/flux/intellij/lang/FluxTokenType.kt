package dev.flux.intellij.lang

import com.intellij.psi.tree.IElementType
import dev.flux.intellij.FluxLanguage
import org.jetbrains.annotations.NonNls

class FluxTokenType(@NonNls debugName: String) : IElementType(debugName, FluxLanguage) {
    override fun toString(): String = "FluxTokenType.${super.toString()}"
}
