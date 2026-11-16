package sh.zeron.android.ui

import androidx.compose.runtime.*
import androidx.compose.ui.platform.LocalContext
import android.content.Intent
import android.net.Uri
import sh.zeron.android.core.AppModel
import sh.zeron.android.core.CloudStore

@Composable
fun ProvidersScreen(model: AppModel, target: String?, onBack: () -> Unit, onOpenAccounts: (AccountsTarget) -> Unit, onOpenCloud: () -> Unit) {
    val workspace by model.workspace.collectAsState()
    val computers = workspace?.devices.orEmpty().filter { it.isExecutionHost }
    var selected by remember { mutableStateOf(target ?: computers.firstOrNull { it.online }?.id ?: computers.firstOrNull()?.id ?: "cloud") }
    val context = LocalContext.current
    val onCloud = selected == "cloud"
    val name = if (onCloud) "Cloud" else model.deviceName(selected)
    CloudPage("DeepSeek", "Configure the official Harness SDK on the run device", onBack) {
        cloudSection("Run device") {
            computers.forEachIndexed { index, device ->
                CloudRow(index, computers.size + if (model.cloudOffered) 1 else 0, device.name, if (selected == device.id) "Selected" else null, onClick = { selected = device.id })
            }
            if (model.cloudOffered) {
                CloudRow(computers.size, computers.size + 1, "Cloud", if (onCloud) "Selected" else null, onClick = { selected = "cloud" })
            }
        }
        cloudSection("DeepSeek", "Install the pinned SDK runtime on $name. Zeron uses an isolated Harness home.") {
            CloudRow(0, 2, "DeepSeek API key", "Configure authentication on $name", onClick = {
                onOpenAccounts(if (onCloud) AccountsTarget.Cloud(CloudStore.agents.first()) else AccountsTarget.Device(selected, name, "deepseek", "DeepSeek"))
            })
            CloudRow(1, 2, "SDK setup guide", "Official DeepSeek Harness documentation", onClick = {
                context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://github.com/deepseek-ai/deepseek-harness")))
            })
        }
        cloudSection("Historical sessions", "Other-provider sessions and settings remain readable, but unsupported. Choose a DeepSeek model explicitly before sending.") {}
    }
}
