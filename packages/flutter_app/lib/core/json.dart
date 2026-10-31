typedef JsonMap = Map<String, dynamic>;

JsonMap jsonMap(Object? value) {
  if (value is JsonMap) return value;
  // StandardMethodCodec returns Map<Object?, Object?>, unlike JSON decoding.
  return value is Map ? value.cast<String, dynamic>() : const {};
}

List<dynamic> jsonList(Object? value) => value is List ? value : const [];
String stringValue(Object? value, [String fallback = '']) =>
    value is String ? value : fallback;
int intValue(Object? value, [int fallback = 0]) =>
    value is num ? value.toInt() : fallback;
bool boolValue(Object? value, [bool fallback = false]) =>
    value is bool ? value : fallback;

extension JsonFields on JsonMap {
  String text(String key, [String fallback = '']) =>
      stringValue(this[key], fallback);
  int integer(String key, [int fallback = 0]) => intValue(this[key], fallback);
  bool flag(String key, [bool fallback = false]) =>
      boolValue(this[key], fallback);
  JsonMap object(String key) => jsonMap(this[key]);
  List<dynamic> array(String key) => jsonList(this[key]);
}
