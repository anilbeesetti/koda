package dev.zed.androidsample

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.zed.greeting.LibraryGreeting

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent { SampleScreen() }
    }
}

@Preview(name = "Default")
@Preview(name = "Large text", fontScale = 1.4f)
@Composable
fun SampleScreen() {
    MaterialTheme {
        Column(Modifier.safeDrawingPadding().padding(24.dp)) {
            Text(stringResource(R.string.app_name), style = MaterialTheme.typography.headlineSmall)
            Text(Greeting.message(BuildConfig.FLAVOR))
            Text(BuildConfig.APPLICATION_ID)
            Text(LibraryGreeting.message())
        }
    }
}
