package dev.flux.intellij.editor

import com.intellij.icons.AllIcons
import com.intellij.ide.structureView.StructureViewBuilder
import com.intellij.ide.structureView.StructureViewModel
import com.intellij.ide.structureView.StructureViewModelBase
import com.intellij.ide.structureView.StructureViewTreeElement
import com.intellij.ide.structureView.TreeBasedStructureViewBuilder
import com.intellij.ide.util.treeView.smartTree.TreeElement
import com.intellij.lang.PsiStructureViewFactory
import com.intellij.navigation.ItemPresentation
import com.intellij.openapi.editor.Editor
import com.intellij.openapi.fileEditor.OpenFileDescriptor
import com.intellij.psi.PsiFile
import javax.swing.Icon

/** Outline of a `.flux` file: its `flow` and `type` declarations, click-to-navigate. */
class FluxStructureViewFactory : PsiStructureViewFactory {
    override fun getStructureViewBuilder(psiFile: PsiFile): StructureViewBuilder =
        object : TreeBasedStructureViewBuilder() {
            override fun createStructureViewModel(editor: Editor?): StructureViewModel =
                FluxStructureViewModel(psiFile)
        }
}

private class FluxStructureViewModel(file: PsiFile) :
    StructureViewModelBase(file, FluxFileRoot(file)),
    StructureViewModel.ElementInfoProvider {
    override fun isAlwaysShowsPlus(element: StructureViewTreeElement): Boolean = element is FluxFileRoot
    override fun isAlwaysLeaf(element: StructureViewTreeElement): Boolean = element !is FluxFileRoot
}

private class FluxFileRoot(private val file: PsiFile) : StructureViewTreeElement {
    override fun getValue(): Any = file
    override fun getPresentation(): ItemPresentation =
        present(file.name, file.fileType.icon)

    override fun getChildren(): Array<TreeElement> {
        val text = file.text
        val children = ArrayList<TreeElement>()
        var offset = 0
        for (line in text.split('\n')) {
            // Declarations sit at column 0.
            if (line.startsWith("flow") && headed(line, "flow")) {
                children.add(decl(line.removePrefix("flow").trim().takeWhile { it != '(' && it != ' ' && it != '-' }, offset, AllIcons.Nodes.Function))
            } else if (line.startsWith("type") && headed(line, "type")) {
                children.add(decl(line.removePrefix("type").trim().takeWhile { it != ' ' }, offset, AllIcons.Nodes.Class))
            }
            offset += line.length + 1
        }
        return children.toTypedArray()
    }

    private fun headed(line: String, kw: String): Boolean =
        line.length == kw.length || line[kw.length] == ' '

    private fun decl(name: String, offset: Int, icon: Icon): TreeElement {
        val element = file.findElementAt(offset)
        return FluxDecl(file, element?.textOffset ?: offset, name.ifBlank { "<anonymous>" }, icon)
    }

    override fun navigate(requestFocus: Boolean) {}
    override fun canNavigate(): Boolean = false
    override fun canNavigateToSource(): Boolean = false
}

private class FluxDecl(
    private val file: PsiFile,
    private val offset: Int,
    private val label: String,
    private val icon: Icon,
) : StructureViewTreeElement {
    override fun getValue(): Any = "$label@$offset"
    override fun getPresentation(): ItemPresentation = present(label, icon)
    override fun getChildren(): Array<TreeElement> = TreeElement.EMPTY_ARRAY

    override fun navigate(requestFocus: Boolean) {
        val vf = file.virtualFile ?: return
        OpenFileDescriptor(file.project, vf, offset).navigate(requestFocus)
    }

    override fun canNavigate(): Boolean = file.virtualFile != null
    override fun canNavigateToSource(): Boolean = file.virtualFile != null
}

private fun present(text: String, icon: Icon?): ItemPresentation = object : ItemPresentation {
    override fun getPresentableText(): String = text
    override fun getLocationString(): String? = null
    override fun getIcon(unused: Boolean): Icon? = icon
}
