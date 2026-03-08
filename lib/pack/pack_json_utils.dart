class PackJsonUtils {
  const PackJsonUtils._();

  static Map<String, dynamic> map(dynamic value) {
    if (value is Map<String, dynamic>) return value;
    if (value is Map) return value.cast<String, dynamic>();
    return <String, dynamic>{};
  }

  static List<dynamic> list(dynamic value) {
    if (value is List) return value;
    return <dynamic>[];
  }
}
