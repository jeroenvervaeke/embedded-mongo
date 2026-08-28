# JNI resolves native methods by class and method name, so R8 renaming NativeBridge -- or its
# methods -- turns into an UnsatisfiedLinkError at run time in a minified build only.
-keepclasseswithmembernames,includedescriptorclasses class io.github.jeroenvervaeke.embeddedmongodb.NativeBridge {
    native <methods>;
}

# The native bridge constructs this class through JNI, by name, and calls this constructor.
-keep class io.github.jeroenvervaeke.embeddedmongodb.EmbeddedMongoException {
    <init>(java.lang.String, int);
}

# BSON picks codecs by looking types up reflectively: a codec provider maps a BSON type to a class
# and instantiates it, so nothing in the codec packages is reachable through a call graph. Without
# these, a release build fails at the first document with a NoSuchMethodException or a
# CodecConfigurationException.
-keep class org.bson.codecs.** { *; }
-keep class org.bson.types.** { *; }
-keep class org.bson.BsonType { *; }

# Property-based codecs read POJO fields and constructors reflectively. Applications that only pass
# Documents never touch this, but the classes are on the classpath either way.
-keepclassmembers class * {
    @org.bson.codecs.pojo.annotations.BsonCreator <init>(...);
    @org.bson.codecs.pojo.annotations.BsonProperty <fields>;
}
