package dev.zed.kotlin.compose

import com.intellij.codeInspection.InspectionSuppressor
import com.intellij.codeInspection.SuppressQuickFix
import com.intellij.codeInsight.completion.CompletionContributor
import com.intellij.codeInsight.completion.CompletionParameters
import com.intellij.codeInsight.completion.CompletionResultSet
import com.intellij.codeInsight.completion.CompletionResult
import com.intellij.codeInsight.completion.CompletionSorter
import com.intellij.codeInsight.completion.InsertionContext
import com.intellij.codeInsight.lookup.LookupElement
import com.intellij.codeInsight.lookup.LookupElementDecorator
import com.intellij.codeInsight.lookup.LookupElementWeigher
import com.intellij.psi.PsiDocumentManager
import com.intellij.psi.PsiElement
import com.intellij.psi.util.PsiTreeUtil
import org.jetbrains.kotlin.analysis.api.KaExperimentalApi
import org.jetbrains.kotlin.analysis.api.analyze
import org.jetbrains.kotlin.analysis.api.types.KaFunctionType
import org.jetbrains.kotlin.kdoc.psi.api.KDoc
import org.jetbrains.kotlin.idea.completion.impl.k2.lookups.factories.NamedArgumentLookupObject
import org.jetbrains.kotlin.lexer.KtTokens
import org.jetbrains.kotlin.name.ClassId
import org.jetbrains.kotlin.name.FqName
import org.jetbrains.kotlin.psi.KtNamedFunction
import org.jetbrains.kotlin.psi.KtCallExpression
import org.jetbrains.kotlin.psi.KtCallableReferenceExpression
import org.jetbrains.kotlin.psi.KtImportDirective
import org.jetbrains.kotlin.psi.KtPackageDirective
import org.jetbrains.kotlin.psi.KtTypeReference
import org.jetbrains.kotlin.psi.KtLambdaExpression
import org.jetbrains.kotlin.psi.KtNameReferenceExpression
import org.jetbrains.kotlin.psi.psiUtil.getQualifiedExpressionForSelector

private val composable = ClassId.topLevel(FqName("androidx.compose.runtime.Composable"))

// Compiled annotation stubs use the resolved class name; source annotations can use aliases.
private fun KtNamedFunction.mayBeComposable(): Boolean = annotationEntries.any { annotation ->
    !containingKtFile.isCompiled || annotation.shortName == null || annotation.shortName == composable.shortClassName
}

private fun isComposableContext(position: PsiElement): Boolean {
    for (element in generateSequence(position) { it.parent }) {
        if (element is KtNamedFunction) {
            return element.mayBeComposable() && analyze(element) { composable in element.symbol.annotations }
        }
        if (element is KtLambdaExpression && analyze(element) { (element.expectedType as? KaFunctionType)?.annotations?.contains(composable) == true }) return true
    }
    return false
}

class ComposeSuppressor : InspectionSuppressor {
    override fun isSuppressedFor(element: PsiElement, toolId: String): Boolean {
        if (toolId != "FunctionName" && toolId != "TestFunctionName") return false
        if (element.node.elementType != KtTokens.IDENTIFIER) return false
        val function = element.parent as? KtNamedFunction ?: return false
        if (!function.mayBeComposable()) return false
        return analyze(function) { composable in function.symbol.annotations }
    }

    override fun getSuppressActions(element: PsiElement?, toolId: String): Array<SuppressQuickFix> = SuppressQuickFix.EMPTY_ARRAY
}

class ComposeCompletionContributor : CompletionContributor() {
    @OptIn(KaExperimentalApi::class)
    override fun fillCompletionVariants(parameters: CompletionParameters, result: CompletionResultSet) {
        // A qualified selector cannot be a named argument.
        val qualified = (parameters.position.parent as? KtNameReferenceExpression)?.getQualifiedExpressionForSelector() != null
        val inComposable = !qualified && isComposableContext(parameters.position)
        val sorters = mutableMapOf<CompletionSorter, CompletionSorter>()
        result.runRemainingContributors(parameters) { original ->
            val candidate = if (inComposable) {
                val sorter = sorters.getOrPut(original.sorter) { original.sorter.weighBefore("kotlin.variableOrFunction", ComposeNamedArgumentWeigher) }
                CompletionResult.wrap(original.lookupElement, original.prefixMatcher, sorter) ?: original
            } else original
            val function = candidate.lookupElement.psiElement as? KtNamedFunction
            val requiredBeforeLambda = function?.takeIf { it.mayBeComposable() }?.let {
                analyze(it) {
                    val symbol = it.symbol
                    val last = symbol.valueParameters.lastOrNull()
                    if (composable !in symbol.annotations || last == null || last.isVararg || last.hasDeclaredDefaultValue || last.returnType !is KaFunctionType) null
                    else symbol.valueParameters.dropLast(1).any { !it.isVararg && !it.hasDeclaredDefaultValue }
                }
            }
            result.passResult(if (requiredBeforeLambda == null) candidate else candidate.withLookupElement(ComposeCall(candidate.lookupElement, requiredBeforeLambda)))
        }
    }
}

private object ComposeNamedArgumentWeigher : LookupElementWeigher("zed.compose.namedArgument") {
    override fun weigh(element: LookupElement): Int {
        if (element.`object` is NamedArgumentLookupObject) return 0
        return 1
    }
}

private class ComposeCall(original: LookupElement, private val requiredBeforeLambda: Boolean) : LookupElementDecorator<LookupElement>(original) {
    override fun handleInsert(context: InsertionContext) {
        val element = context.file.findElementAt(context.startOffset)
        val excluded = generateSequence(element) { it.parent }.any { it is KtImportDirective || it is KtPackageDirective || it is KtTypeReference || it is KtCallableReferenceExpression || it is KDoc }
        super.handleInsert(context)
        if (excluded) return
        val documents = PsiDocumentManager.getInstance(context.project)
        documents.doPostponedOperationsAndUnblockDocument(context.document)
        documents.commitDocument(context.document)
        // Kotlin's import insertion moves the original offset; the tail follows the inserted call.
        val inserted = context.file.findElementAt((context.tailOffset - 1).coerceAtLeast(0)) ?: return
        if (generateSequence(inserted) { it.parent }.any { it is KtCallableReferenceExpression || it is KtTypeReference }) return
        val call = PsiTreeUtil.getParentOfType(inserted, KtCallExpression::class.java) ?: return
        if (call.lambdaArguments.isNotEmpty()) return
        if (!requiredBeforeLambda) call.valueArgumentList?.takeIf { it.arguments.isEmpty() }?.delete()
        documents.commitDocument(context.document)
        val insertion = call.textRange.endOffset
        context.document.insertString(insertion, " { }")
        documents.commitDocument(context.document)
        if (!requiredBeforeLambda) context.editor.caretModel.moveToOffset(insertion + 3)
    }
}
