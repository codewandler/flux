package dev.flux.intellij

import com.intellij.lang.Language

/** The Flux-Lang language. Its ID ("Flux-Lang") is what `language="…"` in plugin.xml binds to. */
object FluxLanguage : Language("Flux-Lang") {
    private fun readResolve(): Any = FluxLanguage
    override fun getDisplayName(): String = "Flux-Lang"
}
