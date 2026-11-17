import UIKit

final class ProvidersViewController: UITableViewController {
    private let app: AppModel
    private var target: String
    static let cloud = "cloud"
    var onSummary: ((_ place: String, _ cloud: Bool, _ names: [String]?) -> Void)?

    init(app: AppModel, target: String? = nil) {
        self.app = app
        self.target = target ?? app.hostOptions.first(where: \.online)?.id ?? app.hostOptions.first?.id ?? Self.cloud
        super.init(style: .insetGrouped)
        title = "DeepSeek"
    }

    required init?(coder: NSCoder) { fatalError() }

    private var targetName: String { target == Self.cloud ? "Cloud" : app.deviceName(target) }

    override func viewDidLoad() {
        super.viewDidLoad()
        tableView.backgroundColor = Palette.background
        navigationItem.largeTitleDisplayMode = .never
        refresh()
    }

    private func refresh() {
        var actions = app.hostOptions.map { host in
            UIAction(title: host.name, state: host.id == target ? .on : .off) { [weak self] _ in
                self?.target = host.id
                self?.refresh()
            }
        }
        if app.cloud.available {
            actions.append(UIAction(title: "Cloud", state: target == Self.cloud ? .on : .off) { [weak self] _ in
                self?.target = Self.cloud
                self?.refresh()
            })
        }
        navigationItem.rightBarButtonItem = actions.isEmpty ? nil : UIBarButtonItem(title: targetName, menu: UIMenu(children: actions))
        onSummary?(targetName, target == Self.cloud, ["DeepSeek"])
        tableView.reloadData()
    }

    override func numberOfSections(in tableView: UITableView) -> Int { 2 }
    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int { section == 0 ? 2 : 1 }
    override func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? { section == 0 ? targetName : "Historical sessions" }
    override func tableView(_ tableView: UITableView, titleForFooterInSection section: Int) -> String? {
        section == 0 ? "Install the pinned official DeepSeek Harness SDK on the run device. Zeron uses its own isolated Harness home." : "Other-provider sessions and settings remain readable, but unsupported. Choose a DeepSeek model explicitly before sending."
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = UITableViewCell(style: .subtitle, reuseIdentifier: nil)
        cell.backgroundColor = Palette.elevated
        cell.textLabel?.textColor = Palette.text
        cell.detailTextLabel?.textColor = Palette.secondary
        cell.detailTextLabel?.numberOfLines = 0
        if indexPath.section == 1 {
            cell.textLabel?.text = "Saved data is preserved"
            cell.selectionStyle = .none
        } else if indexPath.row == 0 {
            cell.textLabel?.text = "DeepSeek API key"
            cell.detailTextLabel?.text = "Configure authentication on \(targetName)"
            cell.accessoryType = .disclosureIndicator
            cell.imageView?.image = BrandMarks.image(for: "deepseek", side: 20)
        } else {
            cell.textLabel?.text = "SDK setup guide"
            cell.detailTextLabel?.text = "Official DeepSeek Harness documentation"
            cell.accessoryType = .disclosureIndicator
        }
        return cell
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        guard indexPath.section == 0 else { return }
        if indexPath.row == 1 {
            UIApplication.shared.open(URL(string: "https://github.com/deepseek-ai/deepseek-harness")!)
            return
        }
        let destination: AgentAccountsViewController.Target = target == Self.cloud
            ? .cloud(CloudStore.agents[0])
            : .device(id: target, name: targetName, harness: "deepseek", label: "DeepSeek")
        navigationController?.pushViewController(AgentAccountsViewController(app: app, target: destination), animated: true)
    }
}
