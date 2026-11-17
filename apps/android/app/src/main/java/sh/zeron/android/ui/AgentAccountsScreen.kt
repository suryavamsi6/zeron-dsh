package sh.zeron.android.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import kotlin.coroutines.cancellation.CancellationException
import sh.zeron.android.core.AppModel

@OptIn(ExperimentalMaterial3ExpressiveApi::class)
@Composable
fun AgentAccountsScreen(model: AppModel, target: AccountsTarget, signIn: Boolean, onBack: () -> Unit) {
    val client by model.client.collectAsState()
    val scope = rememberCoroutineScope()
    var key by remember(target) { mutableStateOf("") }
    var saving by remember(target) { mutableStateOf(false) }
    var message by remember(target) { mutableStateOf("") }
    val supported = target.harness == "deepseek"
    CloudPage("DeepSeek setup", "Official DeepSeek Harness SDK", onBack) {
        cloudSection("Authentication", if (supported) "Provide DEEPSEEK_API_KEY securely to the engine, or save a key here. An explicitly saved key takes precedence. Zeron uses its own isolated Harness home." else "This historical provider is unsupported. Its saved data is preserved; choose DeepSeek explicitly before sending.") {
            if (supported) {
                Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    OutlinedTextField(key, { key = it }, label = { Text("DeepSeek API key") }, visualTransformation = PasswordVisualTransformation(), singleLine = true, enabled = !saving, modifier = Modifier.fillMaxWidth())
                    Button(enabled = !saving && key.isNotBlank(), onClick = {
                        val c = client ?: return@Button
                        if (key.trim().any { it.isWhitespace() }) { message = "Enter an API key without whitespace."; return@Button }
                        saving = true
                        scope.launch {
                            try {
                                when (target) {
                                    is AccountsTarget.Cloud -> { c.vaultPutApiKey("deepseek-key", key.trim()); model.cloud.refreshAccounts() }
                                    is AccountsTarget.Device -> c.deviceAddApiKey(target.deviceId, key.trim())
                                }
                                key = ""
                                message = "DeepSeek API key saved for the next run."
                            } catch (e: CancellationException) {
                                throw e
                            } catch (_: Exception) {
                                message = "Couldn't save the DeepSeek API key. Check the run device and try again."
                            } finally {
                                saving = false
                            }
                        }
                    }) { Text(if (saving) "Saving…" else "Save DeepSeek API key") }
                    if (message.isNotEmpty()) Text(message)
                }
            }
        }
    }
}
