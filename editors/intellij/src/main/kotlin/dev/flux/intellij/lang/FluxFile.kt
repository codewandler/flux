package dev.flux.intellij.lang

import com.intellij.extapi.psi.PsiFileBase
import com.intellij.openapi.fileTypes.FileType
import com.intellij.psi.FileViewProvider
import dev.flux.intellij.FluxFileType
import dev.flux.intellij.FluxLanguage

class FluxFile(viewProvider: FileViewProvider) : PsiFileBase(viewProvider, FluxLanguage) {
    override fun getFileType(): FileType = FluxFileType.INSTANCE
    override fun toString(): String = "Flux-Lang File"
}
