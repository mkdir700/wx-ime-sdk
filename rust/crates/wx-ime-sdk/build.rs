use prost::Message;

fn main() {
    let mut descriptors =
        prost_types::FileDescriptorSet::decode(wx_ime_protocol::FILE_DESCRIPTOR_SET)
            .expect("decode canonical WeType descriptors");
    descriptors
        .file
        .retain(|file| file.package.as_deref() == Some("flurry"));
    assert_eq!(descriptors.file.len(), 1, "canonical file-transfer schema");
    tonic_prost_build::configure()
        .extern_path(".flurry.FileChunk", "::wx_ime_protocol::flurry::FileChunk")
        .extern_path(".flurry.Action", "::wx_ime_protocol::flurry::Action")
        .extern_path(".flurry.FileInfo", "::wx_ime_protocol::flurry::FileInfo")
        .compile_fds(descriptors)
        .expect("compile native file-transfer service");
}
