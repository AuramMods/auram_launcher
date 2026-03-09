import 'dart:io';

import 'package:arcane/arcane.dart';
import 'package:auram_launcher/pack/pack_file_utils.dart';
import 'package:auram_launcher/pack/pack_path_utils.dart';
import 'package:fast_log/fast_log.dart';

class PackServerService {
  static const List<String> _serverSyncDirs = <String>[
    "mods",
    "config",
    "kubejs",
    "scripts",
    "defaultconfigs",
  ];

  const PackServerService._();

  static Future<void> buildServerFromClient({
    required BehaviorSubject<(String, double)?> progressStream,
    required Directory clientMinecraftDir,
    required Directory serverDir,
  }) async {
    info("Building server files from client data");
    verbose("Client source dir: ${clientMinecraftDir.path}");
    verbose("Server target dir: ${serverDir.path}");
    progressStream.add(("Building Server", -1));
    try {
      if (!await serverDir.exists()) {
        await serverDir.create(recursive: true);
        verbose("Created server directory: ${serverDir.path}");
      }

      int total = _serverSyncDirs.length;
      int completed = 0;
      for (String dirName in _serverSyncDirs) {
        String sourcePath = PackPathUtils.joinPath(<String>[
          clientMinecraftDir.path,
          dirName,
        ]);
        String destinationPath = PackPathUtils.joinPath(<String>[
          serverDir.path,
          dirName,
        ]);
        Directory destinationDir = Directory(destinationPath);
        if (await destinationDir.exists()) {
          verbose("Removing existing server directory: $destinationPath");
          await destinationDir.delete(recursive: true);
        }

        Directory sourceDir = Directory(sourcePath);
        if (!await sourceDir.exists()) {
          warn("Server build source missing: $sourcePath");
          completed += 1;
          progressStream.add((
            "Building Server ($completed/$total)",
            completed / total,
          ));
          continue;
        }

        info("Copying $dirName to server");
        await PackFileUtils.copyEntity(
          source: sourceDir,
          destination: destinationPath,
        );
        completed += 1;
        progressStream.add((
          "Building Server ($completed/$total)",
          completed / total,
        ));
        success("Copied $dirName to server");
      }

      success("Server build complete: ${serverDir.path}");
    } on Object catch (e) {
      error("Server build failed: $e");
      rethrow;
    } finally {
      progressStream.add(null);
    }
  }
}
