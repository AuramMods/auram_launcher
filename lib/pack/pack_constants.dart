import 'dart:io';

import 'package:arcane/arcane.dart';
import 'package:auram_launcher/pack/pack_types.dart';
import 'package:fast_log/fast_log.dart';

class PackConstants {
  static const String packRepoOwner = "AuramMods";
  static const String packRepoName = "Auram";

  static const int assumedPackBytes = 1024 * 1024 * 1024;
  static const String mojangVersionManifestUrl =
      "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json";
  static const String mojangAssetObjectBaseUrl =
      "https://resources.download.minecraft.net";
  static const String mojangLibraryBaseUrl = "https://libraries.minecraft.net/";
  static const String forgeMavenBaseUrl = "https://maven.minecraftforge.net/";
  static const String jvmFlagsOver32gb =
      "-Xmx20g -Xms12g -XX:+UseG1GC -XX:+UnlockExperimentalVMOptions -XX:+ParallelRefProcEnabled -XX:+DisableExplicitGC -XX:+AlwaysPreTouch -XX:MaxGCPauseMillis=16 -XX:InitiatingHeapOccupancyPercent=15 -XX:G1HeapWastePercent=8 -XX:G1MixedGCCountTarget=8 -XX:G1MixedGCLiveThresholdPercent=85 -XX:G1RSetUpdatingPauseTimePercent=5 -XX:G1ReservePercent=15 -Dfml.readTimeout=120 -Dfml.loginTimeout=120";
  static const String jvmFlags32gbOrLower =
      "-Xmx9g -Xms9g -XX:+UseG1GC -XX:+ParallelRefProcEnabled -XX:+DisableExplicitGC -XX:+AlwaysPreTouch -XX:MaxGCPauseMillis=50 -XX:InitiatingHeapOccupancyPercent=15 -XX:G1HeapWastePercent=5 -XX:G1MixedGCCountTarget=4 -XX:G1ReservePercent=10 -Dfml.readTimeout=120 -Dfml.loginTimeout=120";

  static Future<String> get jvmFlags => getTotalSystemMemoryGiB()
      .thenRun((gb) => info("System Memory: ~${gb.round()} GB"))
      .then((memGiB) => memGiB > 35 ? jvmFlagsOver32gb : jvmFlags32gbOrLower)
      .thenRun((flags) => verbose("Using Flags: $flags"));

  static const Map<(APlatform, AArch), String>
  jdkDownloads = <(APlatform, AArch), String>{
    (
      APlatform.macos,
      AArch.arm64,
    ): "https://cdn.azul.com/zulu/bin/zulu17.64.17-ca-jdk17.0.18-macosx_aarch64.zip",
    (
      APlatform.macos,
      AArch.x64,
    ): "https://cdn.azul.com/zulu/bin/zulu17.64.17-ca-jdk17.0.18-macosx_x64.zip",
    (APlatform.windows, AArch.x64):
        "https://cdn.azul.com/zulu/bin/zulu17.64.17-ca-jdk17.0.18-win_x64.zip",
    (
      APlatform.windows,
      AArch.arm64,
    ): "https://cdn.azul.com/zulu/bin/zulu17.64.17-ca-jdk17.0.18-win_aarch64.zip",
    (
      APlatform.linux,
      AArch.x64,
    ): "https://cdn.azul.com/zulu/bin/zulu17.64.17-ca-jdk17.0.18-linux_x64.zip",
  };

  static const String packTagsApiUrl =
      "https://api.github.com/repos/$packRepoOwner/$packRepoName/tags?per_page=100";

  static const List<String> protectedMinecraftPaths = <String>[
    "options.txt",
    "shaderpacks/",
    "resourcepacks/",
    "servers.dat",
    "servers.dat_old",
    "journeymap/data/",
    "logs/",
    "crash-reports/",
    "usercache.json",
    "usernamecache.json",
    "patchouli_data.json",
    "config/jei/jei-client.ini",
    "config/embeddium-options.json",
  ];

  static String packTagZipUrl(String tag) =>
      "https://codeload.github.com/$packRepoOwner/$packRepoName/zip/refs/tags/${Uri.encodeComponent(tag)}";

  static Future<double> getTotalSystemMemoryGiB() async =>
      formatBytesToGiB(await getTotalSystemMemoryBytes());

  static Future<int> getTotalSystemMemoryBytes() async {
    if (Platform.isMacOS) {
      final result = await Process.run('sysctl', ['-n', 'hw.memsize']);

      if (result.exitCode != 0) {
        throw Exception('sysctl failed: ${result.stderr}');
      }

      final output = (result.stdout as String).trim();
      final bytes = int.tryParse(output);
      if (bytes == null) {
        throw FormatException('Could not parse macOS memory output: $output');
      }
      return bytes;
    }

    if (Platform.isWindows) {
      final result = await Process.run('powershell.exe', [
        '-NoProfile',
        '-Command',
        '(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory',
      ]);

      if (result.exitCode != 0) {
        throw Exception('PowerShell failed: ${result.stderr}');
      }

      final output = (result.stdout as String).trim();
      final match = RegExp(r'\d+').firstMatch(output);
      if (match == null) {
        throw FormatException('Could not parse Windows memory output: $output');
      }

      return int.parse(match.group(0)!);
    }

    throw UnsupportedError('Only macOS and Windows are implemented here.');
  }

  static double formatBytesToGiB(int bytes) {
    const double gib = 1024 * 1024 * 1024;
    return bytes / gib;
  }

  const PackConstants._();
}
