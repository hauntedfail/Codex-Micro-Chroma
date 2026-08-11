use strict;
use warnings;
use DynaLoader;

my $library = shift @ARGV;
die "media session helper path is required\n" unless defined $library;

my $handle = DynaLoader::dl_load_file($library, 0)
    or die DynaLoader::dl_error() . "\n";
my $symbol = DynaLoader::dl_find_symbol($handle, "chroma_media_sessions_stream")
    or die DynaLoader::dl_error() . "\n";

# Perl invokes installed XSUBs with pTHX_ and CV * arguments. The helper must
# remain a void(void)-style entry point that ignores those arguments and returns
# no value while it blocks on its own run loop.
DynaLoader::dl_install_xsub("main::chroma_media_sessions_stream", $symbol);
chroma_media_sessions_stream();
