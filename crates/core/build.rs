// `sqlx::migrate!` embeds the migrations at compile time; without this, a new file is missed until core rebuilds.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
