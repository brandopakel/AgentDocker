import Foundation

// Name validation is read-only. Setting hasHiddenExtension writes FinderInfo,
// which invalidates strict signature verification on the completed bundle.
let url = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let saved = try url.resourceValues(forKeys: [.isApplicationKey])
guard saved.isApplication == true,
      Bundle(url: url)?.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String == "agentdocker" else {
    fatalError("bundle display metadata failed verification")
}
