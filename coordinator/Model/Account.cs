using System;
using System.Collections.Generic;
using System.ComponentModel.DataAnnotations.Schema;

#pragma warning disable CS8618  
namespace Karenia.Rurikawa.Models.Account {
    public enum AccountKind {
        User,
        Admin,
        Root,
    }

    public class UserAccount {
        public string Username { get; set; }

        public string HashedPassword { get; set; }

        public AccountKind Kind { get; set; }
    }


    /// <summary>
    /// The class used for storing user profiles.
    /// </summary>
    public class Profile {
        public string Username { get; set; }
        public string? Email { get; set; }
        public string? StudentId { get; set; }
    }

    /// <summary>
    /// The class used for storing long-lived Access Tokens and 
    /// Refresh Tokens for users
    /// </summary>
    public class TokenEntry {
        public TokenEntry(
            string username,
            string token,
            DateTimeOffset issuedTime,
            List<string> scope,
            string? tokenName = null,
            string? relatedToken = null,
            DateTimeOffset? expires = null,
            DateTimeOffset? lastUseTime = null,
            bool isSingleUse = false) {
            Username = username;
            TokenName = tokenName;
            Expires = expires;
            Token = token;
            RelatedToken = relatedToken;
            IssuedTime = issuedTime;
            Scope = scope;
            LastUseTime = lastUseTime;
            IsSingleUse = isSingleUse;
        }

        public string Username { get; set; }
        public string? TokenName { get; set; }
        public string Token { get; set; }
        public List<string> Scope { get; set; }
        public string? RelatedToken { get; set; }
        public DateTimeOffset IssuedTime { get; set; }
        public DateTimeOffset? Expires { get; set; }
        public bool IsSingleUse { get; set; }
        public DateTimeOffset? LastUseTime { get; set; }

        public bool IsExpired() {
            DateTimeOffset now = DateTimeOffset.Now;
            if (Expires <= now) return true;
            if (IsSingleUse
                && LastUseTime.HasValue
                && LastUseTime.Value.Add(SingleUseTokenGracePeriod) <= now) return true;
            return false;
        }

        public static readonly TimeSpan SingleUseTokenGracePeriod
            = TimeSpan.FromMinutes(5);
    }
}
