@file:JvmName("PreviewGalleryFacade")

package dev.zed.androidsample

import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.tooling.preview.PreviewParameter
import androidx.compose.ui.tooling.preview.PreviewParameterProvider

@Preview(name = "Plain")
@Composable
fun PlainPreview() { Text("Preview baseline") }

@Preview(name = "Narrow", widthDp = 120)
@Preview(name = "Wide", widthDp = 240)
annotation class PreviewWidths

@PreviewWidths
@Composable
fun WidthPreview() { Text("Multiple preview annotations") }

@Preview(name = "Light", uiMode = 16)
@Preview(name = "Dark", uiMode = 32)
@Composable
fun ThemePreview() { Text("Repeated previews") }

class PreviewNames : PreviewParameterProvider<String> {
    override val values = sequenceOf("First", "Second", "Third")
}

@Preview
@Composable
fun ParameterPreview(@PreviewParameter(PreviewNames::class, limit = 2) name: String) {
    Text(name)
}

@Preview
@Composable
fun BrokenPreview() { error("Expected preview failure") }

@Preview(name = "Large text", fontScale = 1.5f)
@Composable
fun LargeTextPreview() { Text("Larger preview text") }

@Preview
@Composable
private fun PrivatePreview() { Text("Private composable") }
