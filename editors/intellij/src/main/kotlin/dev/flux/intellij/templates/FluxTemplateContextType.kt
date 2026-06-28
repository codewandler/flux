package dev.flux.intellij.templates

import com.intellij.codeInsight.template.TemplateActionContext
import com.intellij.codeInsight.template.TemplateContextType
import dev.flux.intellij.FluxFileType

/** Makes the bundled live templates available inside `.flux` files. */
class FluxTemplateContextType : TemplateContextType("Flux-Lang") {
    override fun isInContext(context: TemplateActionContext): Boolean =
        context.file.fileType == FluxFileType.INSTANCE
}
