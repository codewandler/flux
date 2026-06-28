package dev.flux.intellij

import com.intellij.openapi.fileTypes.LanguageFileType
import javax.swing.Icon

/** Binds the `.flux` extension to [FluxLanguage]. */
class FluxFileType private constructor() : LanguageFileType(FluxLanguage) {
    override fun getName(): String = "Flux-Lang"
    override fun getDescription(): String = "Flux-Lang execution-graph plan"
    override fun getDefaultExtension(): String = "flux"
    override fun getIcon(): Icon = FluxIcons.FILE

    companion object {
        @JvmField
        val INSTANCE = FluxFileType()
    }
}
