import Foundation

var url = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
var values = URLResourceValues()
values.hasHiddenExtension = true
try url.setResourceValues(values)
let saved = try url.resourceValues(forKeys: [.hasHiddenExtensionKey, .isApplicationKey])
guard saved.hasHiddenExtension == true, saved.isApplication == true,
      Bundle(url: url)?.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String == "agentdocker" else {
    fatalError("bundle display metadata failed verification")
}
