import 'dart:io';

import 'package:auram_launcher/pack/pack_constants.dart';
import 'package:auram_launcher/pack/pack_types.dart';

class PackPlatformUtils {
  const PackPlatformUtils._();

  static APlatform currentPlatform() {
    if (Platform.isMacOS) return APlatform.macos;
    if (Platform.isWindows) return APlatform.windows;
    if (Platform.isLinux) return APlatform.linux;
    throw UnsupportedError("Unsupported platform");
  }

  static AArch currentArch() {
    if (Platform.isWindows) {
      String arch = windowsArchToken();
      if (arch.contains("arm64") || arch.contains("aarch64")) {
        return AArch.arm64;
      }
      return AArch.x64;
    }

    if (Platform.isMacOS || Platform.isLinux) {
      ProcessResult result = Process.runSync("uname", ["-m"]);
      if (result.exitCode != 0) {
        throw Exception("Failed to determine architecture: ${result.stderr}");
      }
      String arch = result.stdout.toString().trim().toLowerCase();
      if (arch == "x86_64" || arch == "amd64") return AArch.x64;
      if (arch == "arm64" || arch == "aarch64") return AArch.arm64;
      throw UnsupportedError("Unsupported architecture: $arch");
    }

    throw UnsupportedError("Unsupported platform");
  }

  static String jdkDownload() =>
      PackConstants.jdkDownloads[(currentPlatform(), currentArch())] ??
      (throw UnsupportedError(
        "Unsupported platform/architecture combination!",
      ));

  static String windowsArchToken() {
    String wow64 =
        Platform.environment["PROCESSOR_ARCHITEW6432"]?.toLowerCase() ?? "";
    if (wow64.isNotEmpty) return wow64;
    return Platform.environment["PROCESSOR_ARCHITECTURE"]?.toLowerCase() ?? "";
  }
}
