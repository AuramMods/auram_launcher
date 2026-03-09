import 'dart:io';

class DownloadTarget {
  final Uri uri;
  final File file;
  final int size;

  const DownloadTarget({
    required this.uri,
    required this.file,
    required this.size,
  });
}

class PackTagRef {
  final String name;
  final String sha;

  const PackTagRef({required this.name, required this.sha});
}
