package com.jetbrains.ls.api.features.impl.kotlin.configuration

import com.intellij.openapi.application.Application
import com.intellij.openapi.application.readAction
import com.intellij.openapi.diagnostic.logger
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.findPsiFile
import com.jetbrains.analyzer.bootstrap.AnalyzerContainerBuilder
import com.jetbrains.ls.api.core.LSServer
import com.jetbrains.ls.api.core.project
import com.jetbrains.ls.api.core.util.findVirtualFile
import com.jetbrains.ls.api.features.commands.LSCommandDescriptor
import com.jetbrains.ls.api.features.commands.LSCommandDescriptorProvider
import com.jetbrains.ls.snapshot.api.impl.core.AnalyzerContextKind
import com.jetbrains.ls.snapshot.api.impl.core.LSConfigurationData
import com.jetbrains.ls.snapshot.api.impl.core.WorkspaceComponent
import com.jetbrains.ls.snapshot.api.impl.core.WorkspaceEvent
import com.jetbrains.lsp.implementation.LspHandlerContext
import com.jetbrains.lsp.implementation.throwLspError
import com.jetbrains.lsp.protocol.Commands.ExecuteCommand
import com.jetbrains.lsp.protocol.ErrorCodes
import com.jetbrains.lsp.protocol.URI
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.onStart
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.longOrNull
import org.jetbrains.kotlin.analysis.api.components.resolveToSymbol
import org.jetbrains.kotlin.analysis.api.session.analyze
import org.jetbrains.kotlin.idea.references.mainReference
import org.jetbrains.kotlin.psi.KtFile
import org.jetbrains.kotlin.psi.KtSimpleNameExpression
import org.jetbrains.kotlin.psi.psiUtil.forEachDescendantOfType

internal class KotlinPreparationState {
    val mutex = Mutex()
    var generation = 0L
    var job: Job? = null
}

internal object KotlinPreparationComponent : WorkspaceComponent<KotlinPreparationState> {
    override fun init(configData: LSConfigurationData) = KotlinPreparationState()
    override fun handleEvent(event: WorkspaceEvent, state: KotlinPreparationState) = state
    override suspend fun registerInApplicationContainer(
        builder: AnalyzerContainerBuilder, application: Application,
        state: KotlinPreparationState, contextKind: AnalyzerContextKind,
    ) {}

    override suspend fun registerInProjectContainer(
        builder: AnalyzerContainerBuilder, project: Project,
        state: KotlinPreparationState, contextKind: AnalyzerContextKind,
    ) {}
}

internal object KotlinNavigationPreparation : LSCommandDescriptorProvider {
    override val commandDescriptors = listOf(LSCommandDescriptor("Prepare active Kotlin file", "zed.prepareKotlinFile") { arguments ->
        val server = contextOf<LSServer>()
        if (arguments.size != 2) invalidArguments("Expected a generation and a document URI, or null to stop")
        val generation = (arguments.first() as? JsonPrimitive)?.takeUnless { it.isString }?.longOrNull?.takeIf { it > 0 }
            ?: invalidArguments("Expected a positive generation")
        val argument = arguments.last()
        val uri = when {
            argument == JsonNull -> null
            argument is JsonPrimitive && argument.isString -> URI(argument.content)
            else -> invalidArguments("Expected a document URI string or null")
        }
        if (uri != null && uri !in server.documents.openDocuments()) invalidArguments("The document must be open")
        val state = server.get(KotlinPreparationComponent)
        state.mutex.withLock {
            if (generation > state.generation || generation == state.generation && uri == null) {
                state.job?.cancelAndJoin()
                state.generation = generation
                state.job = uri?.let {
                    CoroutineScope(server.handlersContext).launch {
                        server.documentUpdated.onStart { emit(Unit) }.collectLatest {
                            server.indexUpdated.onStart { emit(Unit) }.conflate().collect {
                                if (uri !in server.documents.openDocuments()) return@collect
                                try {
                                    server.withAnalysisContext {
                                        readAction {
                                            val file = uri.findVirtualFile()?.findPsiFile(project) as? KtFile ?: return@readAction
                                            analyze(file) {
                                                file.forEachDescendantOfType<KtSimpleNameExpression> { it.mainReference.resolveToSymbol() }
                                            }
                                        }
                                    }
                                } catch (exception: CancellationException) {
                                    throw exception
                                } catch (exception: Exception) {
                                    logger<KotlinNavigationPreparation>().warn("Unable to prepare active Kotlin file", exception)
                                }
                            }
                        }
                    }
                }
            }
        }
        JsonNull
    })

    context(_: LspHandlerContext)
    private fun invalidArguments(message: String): Nothing =
        throwLspError(ExecuteCommand, message, Unit, ErrorCodes.InvalidParams, null)
}
