import UIKit

final class AgentAccountsViewController: UIViewController {
    enum Target {
        case cloud(CloudStore.Agent)
        case device(id: String, name: String, harness: String, label: String)
    }

    private let app: AppModel
    private let target: Target
    private let key = UITextField()
    private let status = UILabel()
    private let save = UIButton(type: .system)
    private var saving = false
    var onChange: (([DeviceAccountInfo]) -> Void)?

    init(app: AppModel, target: Target, accounts: [DeviceAccountInfo]? = nil, signIn: Bool = false) {
        self.app = app
        self.target = target
        super.init(nibName: nil, bundle: nil)
        title = "DeepSeek setup"
    }

    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        let supported: Bool
        switch target {
        case let .cloud(agent): supported = agent.harness == "deepseek"
        case let .device(_, _, harness, _): supported = harness == "deepseek"
        }
        let guidance = UILabel()
        guidance.numberOfLines = 0
        guidance.font = Fonts.ui(.sans, 15)
        guidance.textColor = Palette.secondary
        guidance.text = supported
            ? "Use the official DeepSeek Harness SDK on the run device. Add a DeepSeek API key here, or provide DEEPSEEK_API_KEY securely to the engine. A saved key takes precedence; the SDK uses Zeron's isolated Harness home."
            : "This historical provider is unsupported. Its sessions and settings are preserved. Choose a DeepSeek model explicitly before sending."
        key.isSecureTextEntry = true
        key.textContentType = .password
        key.autocapitalizationType = .none
        key.autocorrectionType = .no
        key.placeholder = "DeepSeek API key"
        key.borderStyle = .roundedRect
        key.accessibilityIdentifier = "deepseek-api-key"
        key.isHidden = !supported
        save.setTitle("Save DeepSeek API key", for: .normal)
        save.isHidden = !supported
        save.addAction(UIAction { [weak self] _ in self?.saveKey() }, for: .touchUpInside)
        status.numberOfLines = 0
        status.textColor = Palette.secondary
        let guide = UIButton(type: .system)
        guide.setTitle("SDK setup guide", for: .normal)
        guide.addAction(UIAction { _ in UIApplication.shared.open(URL(string: "https://github.com/deepseek-ai/deepseek-harness")!) }, for: .touchUpInside)
        let stack = UIStackView(arrangedSubviews: [guidance, guide, key, save, status])
        stack.axis = .vertical
        stack.spacing = 20
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 24),
            stack.leadingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -24),
        ])
        if navigationController?.viewControllers.first === self {
            navigationItem.rightBarButtonItem = UIBarButtonItem(systemItem: .done, primaryAction: UIAction { [weak self] _ in self?.dismiss(animated: true) })
        }
    }

    private func saveKey() {
        guard !saving else { return }
        let value = (key.text ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, !value.contains(where: \.isWhitespace) else {
            status.text = "Enter a DeepSeek API key without whitespace."
            return
        }
        guard let client = app.client else {
            status.text = "Connect to the run device before saving a key."
            return
        }
        saving = true
        save.isEnabled = false
        Task { @MainActor in
            defer { saving = false; save.isEnabled = true }
            do {
                switch target {
                case .cloud:
                    try await client.vaultPutApiKey(provider: "deepseek-key", key: value)
                    app.cloud.refreshAccounts()
                case let .device(id, _, harness, _):
                    guard harness == "deepseek" else { return }
                    let accounts = try await client.deviceAddApiKey(deviceId: id, key: value)
                    onChange?(accounts)
                }
                key.text = ""
                status.text = "DeepSeek API key saved for the next run."
            } catch {
                status.text = "Couldn't save the DeepSeek API key. Check the run device and try again."
            }
        }
    }
}
