BEGIN { @INC = grep { m{\A/System/Library/Perl/} } @INC; }
use strict;
use warnings;
use Errno qw(ENOENT);
use Fcntl qw(:mode O_RDONLY O_NOFOLLOW);
use File::stat qw(lstat);
use User::pwent qw(getpwuid);

sub check_acl {
    my ($path) = @_;
    open(my $acl, '-|', '/bin/ls', '-ldBen', $path)
        or die "Cannot check PIN configuration permissions\n";
    defined(readline $acl) or die "Missing PIN configuration permissions\n";
    my $read_only = qr/(?:read(?:attr|extattr|security)?|list|search|execute|synchronize)/;
    while (my $entry = <$acl>) {
        die "Unsafe PIN configuration permissions\n"
            unless $entry =~ /^\s*\d+: \S+ (?:inherited )?(?:deny [\w,]+|allow $read_only(?:,$read_only)*)\s*$/;
    }
    close($acl) or die "Cannot check PIN configuration permissions\n";
}

sub protected_entry {
    my ($path, $directory) = @_;
    my $info = lstat($path);
    return 0 if !$info && $! == ENOENT;
    die "Cannot check PIN configuration\n" unless $info;
    die "Unsafe PIN configuration\n" unless $info->uid == $>
        && ($info->mode & (S_IWGRP | S_IWOTH)) == 0
        && ($directory ? S_ISDIR($info->mode) : S_ISREG($info->mode));
    check_acl($path);
    return 1;
}

sub offline_pin_clear {
    my ($home, $names) = @_;
    my ($profile, $authority, $legacy) = @$names;
    for ($profile, $authority, $legacy) {
        die "Invalid PIN configuration name\n"
            unless defined && length && !m{[/\0]} && $_ ne '.' && $_ ne '..';
    }
    my $dir = $home;
    die "Missing home directory\n" unless protected_entry($dir, 1);
    for ('Library', 'Preferences', $profile) {
        $dir .= "/$_";
        return 1 unless protected_entry($dir, 1);
    }
    my $has_legacy = protected_entry("$dir/$legacy", 0);
    # Only the service interprets an existing legacy configuration.
    return !$has_legacy unless protected_entry("$dir/$authority", 0);
    sysopen(my $file, "$dir/$authority", O_RDONLY | O_NOFOLLOW)
        or die "Cannot read PIN configuration\n";
    # An empty authority written by the service contains exactly this one field.
    my $empty = "unlock_pin = ''\n";
    my $count = sysread($file, my $line, length($empty) + 1);
    die "Cannot read PIN configuration\n" unless defined $count;
    return 0 unless $line eq $empty || $line eq "unlock_pin = \"\"\n";
    my $more = sysread($file, my $rest, 1);
    die "Cannot finish reading PIN configuration\n" unless defined $more;
    return $more == 0;
}

sub root_pin_clear {
    die "Administrator authorization required\n" unless $> == 0;
    my $root = getpwuid(0);
    die "Unknown root profile\n" unless $root
        && ($root->dir eq '/var/root' || $root->dir eq '/private/var/root');
    for ('/private', '/private/var') {
        die "Missing root profile directory\n" unless protected_entry($_, 1);
    }
    return offline_pin_clear('/private/var/root', \@_);
}
