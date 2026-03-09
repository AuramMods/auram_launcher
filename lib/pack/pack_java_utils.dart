import 'dart:io';

import 'package:auram_launcher/pack/pack_path_utils.dart';
import 'package:fast_log/fast_log.dart';

class PackJavaUtils {
  const PackJavaUtils._();

  static Future<String> resolveJavaExecutable({
    required Directory javaDir,
  }) async {
    verbose("Resolving Java executable inside ${javaDir.path}");
    List<String> candidates = <String>[
      if (Platform.isWindows)
        "${javaDir.path}${Platform.pathSeparator}bin${Platform.pathSeparator}java.exe",
      if (Platform.isWindows)
        "${javaDir.path}${Platform.pathSeparator}jre${Platform.pathSeparator}bin${Platform.pathSeparator}java.exe",
      if (!Platform.isWindows)
        "${javaDir.path}${Platform.pathSeparator}bin${Platform.pathSeparator}java",
      if (!Platform.isWindows)
        "${javaDir.path}${Platform.pathSeparator}jre${Platform.pathSeparator}bin${Platform.pathSeparator}java",
      if (Platform.isMacOS)
        "${javaDir.path}${Platform.pathSeparator}Contents${Platform.pathSeparator}Home${Platform.pathSeparator}bin${Platform.pathSeparator}java",
    ];

    for (String path in candidates) {
      File file = File(path);
      if (await file.exists()) {
        success("Resolved Java executable: ${file.path}");
        return file.path;
      }
    }

    await for (FileSystemEntity entity in javaDir.list(
      recursive: true,
      followLinks: false,
    )) {
      if (entity is! File) continue;
      String fileName = PackPathUtils.basename(entity.path).toLowerCase();
      if (fileName != "java" && fileName != "java.exe") continue;
      String parentName = PackPathUtils.basename(
        entity.parent.path,
      ).toLowerCase();
      if (parentName != "bin") continue;
      success("Resolved Java executable: ${entity.path}");
      return entity.path;
    }

    error("Failed to locate Java executable in ${javaDir.path}");
    throw Exception("Could not locate a java executable in ${javaDir.path}");
  }

  static Future<void> ensureExecutable({required String binaryPath}) async {
    if (Platform.isWindows) {
      verbose("Skipping chmod on Windows for $binaryPath");
      return;
    }
    verbose("Ensuring executable bit on $binaryPath");
    ProcessResult result = await Process.run("chmod", ["+x", binaryPath]);
    if (result.exitCode != 0) {
      error("chmod +x failed for $binaryPath: ${result.stderr}");
      throw ProcessException(
        "chmod",
        ["+x", binaryPath],
        result.stderr.toString(),
        result.exitCode,
      );
    }
    success("Executable bit set: $binaryPath");
  }
}
