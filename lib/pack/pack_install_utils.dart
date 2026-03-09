import 'dart:io';

import 'package:auram_launcher/pack/pack_file_utils.dart';
import 'package:auram_launcher/pack/pack_path_utils.dart';
import 'package:fast_log/fast_log.dart';

class PackInstallUtils {
  const PackInstallUtils._();

  static Future<void> installExtractedDirectory({
    required Directory extractDir,
    required Directory installDir,
  }) async {
    info(
      "Installing extracted content: ${extractDir.path} -> ${installDir.path}",
    );
    List<Directory> topLevelDirs = await extractDir
        .list(followLinks: false)
        .where((entity) => entity is Directory)
        .cast<Directory>()
        .toList();
    Directory sourceRoot = topLevelDirs.length == 1
        ? topLevelDirs.first
        : extractDir;

    if (await installDir.exists()) {
      verbose("Cleaning existing install directory: ${installDir.path}");
      await installDir.delete(recursive: true);
    }

    if (sourceRoot.path != extractDir.path) {
      try {
        await sourceRoot.rename(installDir.path);
        success("Installed extracted content by rename: ${installDir.path}");
        return;
      } catch (_) {}
    }

    verbose(
      "Installing extracted content by recursive copy: ${installDir.path}",
    );
    await installDir.create(recursive: true);
    await for (FileSystemEntity child in sourceRoot.list(followLinks: false)) {
      await PackFileUtils.copyEntity(
        source: child,
        destination:
            "${installDir.path}${Platform.pathSeparator}${PackPathUtils.basename(child.path)}",
      );
    }
    success("Installed extracted content: ${installDir.path}");
  }
}
